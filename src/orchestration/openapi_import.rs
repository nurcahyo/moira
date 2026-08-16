//! OpenAPI 3.x document parsing for the skill-import pipeline (plan 12 §5, workstream H).
//!
//! **Pure, no I/O** — mirrors `security::ssrf::is_denied_ip`: given a `serde_json::Value`
//! this module returns a plain data structure or an error, so every shape (cap
//! enforcement, `$ref` resolution, skill-key derivation) is unit-testable without a
//! network or a database. The caller (`application::AgentPlatformService::import_skills`)
//! is responsible for everything this module deliberately does not do: SSRF-validating
//! the resolved server URL, and persisting the result.
//!
//! # What is derived, and what is not
//!
//! - One [`ParsedOperation`] per `(path, method)` pair under `paths`, for the five methods
//!   `skill_http_executors.method` accepts (`GET`/`POST`/`PUT`/`PATCH`/`DELETE`) —
//!   `options`/`head`/`trace` operations are skipped, since the executor table's own
//!   check constraint could never store them.
//! - `params_schema` flattens `parameters` (path/query/header) into top-level JSON-Schema
//!   properties, and — if present — nests `requestBody`'s `application/json` schema under
//!   a [`DEFAULT_BODY_PROPERTY`] property. Both parameter lists are read: the path item's,
//!   which OpenAPI says every operation under that path inherits, then the operation's own,
//!   which overrides an inherited parameter with the same `(name, in)` pair. This is a
//!   deliberate simplification, not a full OpenAPI-to-JSON-Schema translator: two parameters
//!   that share a name across different `in` locations (e.g. a path param and a query param
//!   both named `id`) still collide in the flattened property list, and the later one
//!   silently wins.
//! - A parameter *can* be named `body` — OpenAPI puts nothing off limits — so the request
//!   body can lose that name to a parameter. It is then nested under the first free name
//!   from `request_body`, `request_body_2`, … and the schema records which property that
//!   was under [`BODY_PROPERTY_ANNOTATION`]. See [`body_property_name`]: the executor reads
//!   the answer back out of the schema rather than assuming one, which is what keeps the
//!   two halves from disagreeing.
//! - `$ref` is resolved **exactly one level deep**, against the same document, for
//!   parameter objects and for the request body's media-type schema — the two shapes
//!   real-world specs use `$ref` for almost universally (shared parameter components,
//!   named request/response schemas). A `$ref` nested *inside* an already-resolved
//!   schema (e.g. a property whose own schema is `{"$ref": ...}`) is left exactly as
//!   written: still valid JSON Schema syntax, just not further inlined. Only local
//!   (`#/...`) references are resolved; an external `$ref` (a URL or a file path) is left
//!   untouched, matching the same "leave nested refs alone" posture.
//!
//! # The 300-operation cap (§5 decision 23)
//!
//! [`MAX_IMPORT_OPERATIONS`] is enforced **before** any per-operation derivation work: the
//! first pass only enumerates `(path, method)` pairs, which is cheap even for a
//! pathologically large document, so a spec far over the cap fails fast rather than after
//! building 10,000 `params_schema` values it is about to discard.
//!
//! # The byte budget, and why the operation cap was not enough
//!
//! An operation count bounds *how many* schemas are derived, not *how big* they are, and the
//! two are independent here because `$ref` resolution **deep-clones** the referenced schema
//! once per operation. A document at the 2 MiB admin body limit
//! (`ADMIN_BODY_LIMIT_BYTES`, `src/http/mod.rs`) can hold one ~2 MiB component schema and 300
//! ~80-byte path entries that each `$ref` it — comfortably inside both the body limit and the
//! operation cap — and the derivation below turns that into ~585 MiB of JSON text held at
//! once, several times that as live [`serde_json::Value`] nodes, then 300 jsonb binds in one
//! transaction, a response body of the same size, and (with an `Idempotency-Key`) one more
//! copy serialised into a single jsonb column. `{"$ref": "#/paths"}` reaches the same
//! amplification with no large component at all. One request, process-wide OOM, every tenant.
//!
//! Three caps close it, and the middle one is the load-bearing one:
//!
//! - [`MAX_DOCUMENT_BYTES`] on the input, checked before anything is derived.
//! - [`MAX_TOTAL_SCHEMA_BYTES`] on the derived schemas **in aggregate** — this is what bounds
//!   the peak allocation, the transaction and the response, because it is charged across
//!   operations rather than within one.
//! - [`MAX_OPERATION_SCHEMA_BYTES`] on any single operation, so one absurd `params_schema`
//!   cannot be stored even when it fits the aggregate.
//!
//! Every charge is measured on the **referenced** value and refused *before* the clone that
//! would materialise it — measuring after the clone would be measuring the damage. Sizes are
//! serialised byte lengths counted through a discarding writer, so nothing is allocated to
//! find out that it is too big.

use std::{collections::HashSet, fmt, io};

use serde_json::{Map, Value, json};

use crate::domain::HttpMethod;

/// Largest number of operations a single import accepts (§5 decision 23). A spec over this
/// is rejected outright — never silently truncated — so the operator learns the true
/// operation count and can split the import rather than discover a partial one later.
pub const MAX_IMPORT_OPERATIONS: usize = 300;

/// Largest serialised input document a single import accepts, a quarter of the 2 MiB admin
/// body limit the HTTP layer already applies. Deliberately below that limit rather than equal
/// to it: this module is pure and callable without going through Axum, so it must carry its
/// own input bound, and the tighter the input the smaller the worst case the two caps below
/// have to absorb.
pub const MAX_DOCUMENT_BYTES: usize = 512 * 1024;

/// Largest derived `params_schema` for any one operation. A schema this size is already far
/// past what a model can usefully be shown; the cap exists so one operation cannot store a
/// row that no reader can afford to load.
pub const MAX_OPERATION_SCHEMA_BYTES: usize = 64 * 1024;

/// Largest derived `params_schema` total across every operation in one import — the cap that
/// actually bounds the `$ref` amplification, since the whole failure mode is one big schema
/// cloned three hundred times, each clone individually modest.
pub const MAX_TOTAL_SCHEMA_BYTES: usize = 2 * 1024 * 1024;

/// The `params_schema` property a JSON `requestBody` is nested under when no parameter has
/// already taken the name.
pub const DEFAULT_BODY_PROPERTY: &str = "body";

/// Schema-level annotation naming the property that actually carries the request body.
///
/// Written by [`build_params_schema`] **only** when the request body could not have
/// [`DEFAULT_BODY_PROPERTY`], so the overwhelmingly common schema is byte-for-byte what it
/// was before this key existed — nothing new is put in front of a provider that has never
/// needed to tolerate it. Absence therefore carries meaning, and [`body_property_name`] is
/// where that meaning is spelled out.
///
/// `x-`-prefixed after the OpenAPI extension convention. JSON Schema ignores keywords it does
/// not recognise, so this changes nothing about how the schema validates.
pub const BODY_PROPERTY_ANNOTATION: &str = "x-moira-body-property";

/// Which property of a derived `params_schema` carries the HTTP request body, if any.
///
/// **The single place that question is answered.** [`build_params_schema`] picks the name and
/// `orchestration::skill_tool::HttpSkillTool` finds it again at call time by calling *this*
/// function — neither side spells a property name out, so neither can drift from the other.
/// It matters because the two used to: the importer learned to rename a colliding body to
/// `request_body` while the executor still hard-coded `"body"`, which inverted the dispatch
/// outright — the request body went into the query string and the scalar parameter went into
/// the JSON body.
///
/// The rule, in precedence order:
///
/// 1. [`BODY_PROPERTY_ANNOTATION`], when it names a property that exists. A stale or
///    misspelled annotation falls through rather than naming a body that is not there.
/// 2. [`DEFAULT_BODY_PROPERTY`], when present. This is both the ordinary un-annotated case
///    and the compatibility path for `params_schema` values stored before the annotation
///    existed.
/// 3. Otherwise `None` — the operation declares no request body.
///
/// Note what rule 2 cannot distinguish: a parameter genuinely named `body` on an operation
/// with *no* `requestBody` still reads as the request body. Closing that needs a positive
/// "this operation has no body" marker on every schema, which the stored rows predating it
/// could not carry; it is a strictly smaller wrong than the inversion above, and unchanged
/// by this function.
pub fn body_property_name(params_schema: &Value) -> Option<&str> {
    let object = params_schema.as_object()?;
    let properties = object.get("properties").and_then(Value::as_object)?;
    if let Some(annotated) = object.get(BODY_PROPERTY_ANNOTATION).and_then(Value::as_str)
        && let Some((name, _)) = properties.get_key_value(annotated)
    {
        return Some(name.as_str());
    }
    properties
        .get_key_value(DEFAULT_BODY_PROPERTY)
        .map(|(name, _)| name.as_str())
}

/// The first property name free for the request body: [`DEFAULT_BODY_PROPERTY`], then
/// `request_body`, then `request_body_2`, `request_body_3`, … Terminates because each
/// candidate it rejects is a distinct key already in a finite map.
///
/// The counter is not decoration. `request_body` alone was the previous fallback, and a spec
/// carrying parameters named both `body` and `request_body` plus a `requestBody` would have
/// silently overwritten one of the three.
fn free_body_property_name(properties: &Map<String, Value>) -> String {
    if !properties.contains_key(DEFAULT_BODY_PROPERTY) {
        return DEFAULT_BODY_PROPERTY.to_string();
    }
    if !properties.contains_key("request_body") {
        return "request_body".to_string();
    }
    (2..)
        .map(|suffix| format!("request_body_{suffix}"))
        .find(|candidate| !properties.contains_key(candidate))
        .expect("a finite property map always leaves a suffixed name free")
}

/// The only HTTP methods `skill_http_executors.method` can store, in the fixed order
/// operations are enumerated — this is what makes skill-key deduplication deterministic
/// for a document that repeats the same `operationId` across methods.
const RECOGNIZED_METHODS: [(&str, HttpMethod); 5] = [
    ("get", HttpMethod::Get),
    ("post", HttpMethod::Post),
    ("put", HttpMethod::Put),
    ("patch", HttpMethod::Patch),
    ("delete", HttpMethod::Delete),
];

/// Longest generated `skill_key`, matching `skills_skill_key_valid`'s 128-character cap
/// (`migrations/0031_agent_platform.sql`). Kept a little under 128 so a dedupe suffix
/// (`-2`, `-3`, …) always fits without a second truncation pass.
const MAX_SKILL_KEY_LEN: usize = 120;

/// Longest generated `display_name`, matching `skills.display_name varchar(200)`.
const MAX_DISPLAY_NAME_LEN: usize = 200;

/// Why [`parse_openapi_document`] refused a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenApiImportError {
    /// The top-level JSON value is not an object.
    NotAnObject,
    /// No `openapi: "3.x"` field — Swagger 2.0 and unversioned documents both land here.
    UnsupportedVersion,
    /// No `servers[0].url`, or it is present but empty.
    MissingServerUrl,
    /// `servers[0].url` is not a parseable absolute URL.
    InvalidServerUrl(String),
    /// `paths` is absent, or it names no `(path, method)` pair this module recognizes.
    NoOperations,
    /// More `(path, method)` pairs than [`MAX_IMPORT_OPERATIONS`] allows.
    TooManyOperations { found: usize, cap: usize },
    /// The serialised document is larger than [`MAX_DOCUMENT_BYTES`].
    DocumentTooLarge { bytes: usize, cap: usize },
    /// One operation's derived `params_schema` exceeds [`MAX_OPERATION_SCHEMA_BYTES`].
    /// Carries the `skill_key` so the operator can find the offending operation in a
    /// three-hundred-operation document.
    OperationSchemaTooLarge {
        skill_key: String,
        bytes: usize,
        cap: usize,
    },
    /// The derived `params_schema` values total more than [`MAX_TOTAL_SCHEMA_BYTES`]. Reported
    /// with the total charged at the moment the budget ran out, which is a lower bound on what
    /// the full import would have cost — the derivation stops there rather than continuing to
    /// find out.
    TotalSchemaTooLarge { bytes: usize, cap: usize },
}

impl fmt::Display for OpenApiImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAnObject => write!(f, "the document is not a JSON object"),
            Self::UnsupportedVersion => {
                write!(f, "the document does not declare an OpenAPI 3.x version")
            }
            Self::MissingServerUrl => write!(f, "the document declares no server URL"),
            Self::InvalidServerUrl(url) => write!(f, "the server URL '{url}' is not valid"),
            Self::NoOperations => write!(f, "the document defines no importable operations"),
            Self::TooManyOperations { found, cap } => write!(
                f,
                "the document defines {found} operations, which exceeds the {cap}-operation \
                 import cap"
            ),
            Self::DocumentTooLarge { bytes, cap } => write!(
                f,
                "the document is {bytes} bytes, which exceeds the {cap}-byte import limit"
            ),
            Self::OperationSchemaTooLarge {
                skill_key,
                bytes,
                cap,
            } => write!(
                f,
                "operation '{skill_key}' derives a {bytes}-byte parameter schema, which \
                 exceeds the {cap}-byte per-operation limit"
            ),
            Self::TotalSchemaTooLarge { bytes, cap } => write!(
                f,
                "the document's operations derive at least {bytes} bytes of parameter \
                 schemas, which exceeds the {cap}-byte total limit"
            ),
        }
    }
}

impl std::error::Error for OpenApiImportError {}

/// One `(path, method)` operation, fully derived and ready to become a `skills` row plus a
/// `skill_http_executors` row.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedOperation {
    pub skill_key: String,
    pub display_name: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub params_schema: Value,
    pub method: HttpMethod,
    /// The OpenAPI path template, e.g. `/v1/orders/{order_id}` — `{placeholder}` syntax
    /// preserved as-is, appended to the validated base URL to form `url_template`.
    pub path: String,
}

/// The result of a successful parse: the server URL every operation is relative to, and
/// every derived operation.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedImport {
    /// `servers[0].url`, trimmed of a trailing slash. **Not yet SSRF-validated** — the
    /// caller must run this through `security::ssrf::validate_outbound_url` before using
    /// it for anything, exactly as this module's own doc comments say.
    pub base_url: String,
    pub operations: Vec<ParsedOperation>,
}

/// Parses and derives every importable operation from `document`. Enforces
/// [`MAX_IMPORT_OPERATIONS`], [`MAX_DOCUMENT_BYTES`], [`MAX_OPERATION_SCHEMA_BYTES`] and
/// [`MAX_TOTAL_SCHEMA_BYTES`], but performs no SSRF check and no I/O — see the module docs.
pub fn parse_openapi_document(document: &Value) -> Result<ParsedImport, OpenApiImportError> {
    let root = document
        .as_object()
        .ok_or(OpenApiImportError::NotAnObject)?;

    // Before anything is read out of the document, let alone derived from it. `serialized_len`
    // allocates nothing, so refusing an over-large document costs one pass over a value the
    // caller already holds.
    let document_bytes = serialized_len(document);
    if document_bytes > MAX_DOCUMENT_BYTES {
        return Err(OpenApiImportError::DocumentTooLarge {
            bytes: document_bytes,
            cap: MAX_DOCUMENT_BYTES,
        });
    }

    let version_ok = root
        .get("openapi")
        .and_then(Value::as_str)
        .is_some_and(|version| version.starts_with("3."));
    if !version_ok {
        return Err(OpenApiImportError::UnsupportedVersion);
    }

    let base_url = root
        .get("servers")
        .and_then(Value::as_array)
        .and_then(|servers| servers.first())
        .and_then(|server| server.get("url"))
        .and_then(Value::as_str)
        .map(|url| url.trim().trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty())
        .ok_or(OpenApiImportError::MissingServerUrl)?;
    if url::Url::parse(&base_url).is_err() {
        return Err(OpenApiImportError::InvalidServerUrl(base_url));
    }

    let paths = root
        .get("paths")
        .and_then(Value::as_object)
        .ok_or(OpenApiImportError::NoOperations)?;

    // First pass: enumerate every `(path, method)` pair. Cheap — no schema derivation yet —
    // so the cap check below runs before any per-operation work.
    let mut path_keys: Vec<&String> = paths.keys().collect();
    path_keys.sort();
    let mut raw_operations: Vec<(&str, &Value, &str, &Value, HttpMethod)> = Vec::new();
    for path in path_keys {
        let Some(item) = paths.get(path) else {
            continue;
        };
        let Some(item_obj) = item.as_object() else {
            continue;
        };
        for (method_name, method) in RECOGNIZED_METHODS {
            if let Some(operation) = item_obj.get(method_name)
                && operation.is_object()
            {
                raw_operations.push((path.as_str(), item, method_name, operation, method));
            }
        }
    }

    if raw_operations.is_empty() {
        return Err(OpenApiImportError::NoOperations);
    }
    if raw_operations.len() > MAX_IMPORT_OPERATIONS {
        return Err(OpenApiImportError::TooManyOperations {
            found: raw_operations.len(),
            cap: MAX_IMPORT_OPERATIONS,
        });
    }

    let mut used_keys: HashSet<String> = HashSet::new();
    let mut budget = SchemaBudget::default();
    let operations = raw_operations
        .into_iter()
        .map(|(path, path_item, method_name, operation, method)| {
            build_operation(
                document,
                path,
                path_item,
                method_name,
                operation,
                method,
                &mut used_keys,
                &mut budget,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ParsedImport {
        base_url,
        operations,
    })
}

/// The running byte charge for one import's derived `params_schema` values.
///
/// It is threaded through the whole import rather than reset per operation because the attack
/// it exists to stop is *per-operation-modest, aggregate-enormous*: three hundred clones of a
/// 1.9 MiB schema pass any per-operation cap that a legitimate large schema also passes. The
/// per-operation figure is kept alongside so a single absurd operation is still nameable.
#[derive(Debug, Default)]
struct SchemaBudget {
    operation_bytes: usize,
    total_bytes: usize,
}

impl SchemaBudget {
    fn begin_operation(&mut self) {
        self.operation_bytes = 0;
    }

    /// Charges `bytes` against both caps. Call this with the size of the value **about to be
    /// cloned**, never with the size of the clone: the allocation is the harm, so a check that
    /// runs after it has already happened is a report, not a guard.
    fn charge(&mut self, bytes: usize, skill_key: &str) -> Result<(), OpenApiImportError> {
        self.operation_bytes = self.operation_bytes.saturating_add(bytes);
        self.total_bytes = self.total_bytes.saturating_add(bytes);
        if self.operation_bytes > MAX_OPERATION_SCHEMA_BYTES {
            return Err(OpenApiImportError::OperationSchemaTooLarge {
                skill_key: skill_key.to_string(),
                bytes: self.operation_bytes,
                cap: MAX_OPERATION_SCHEMA_BYTES,
            });
        }
        if self.total_bytes > MAX_TOTAL_SCHEMA_BYTES {
            return Err(OpenApiImportError::TotalSchemaTooLarge {
                bytes: self.total_bytes,
                cap: MAX_TOTAL_SCHEMA_BYTES,
            });
        }
        Ok(())
    }
}

/// The serialised byte length of `value`, counted **without materialising the string** — the
/// entire point is to avoid allocating a copy of something that is about to be refused, so
/// `to_string().len()` would defeat the guard on exactly the input it is meant to stop.
///
/// `serde_json::to_writer` over a `Value` can only fail on an I/O error from the writer, and
/// this writer never errors, so the `Result` is genuinely unreachable rather than merely
/// unlikely; treating it as zero bytes would under-charge, so it saturates the counter instead.
fn serialized_len(value: &Value) -> usize {
    #[derive(Default)]
    struct ByteCounter(usize);

    impl io::Write for ByteCounter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0 = self.0.saturating_add(buf.len());
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut counter = ByteCounter::default();
    match serde_json::to_writer(&mut counter, value) {
        Ok(()) => counter.0,
        Err(_) => usize::MAX,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_operation<'a>(
    root: &'a Value,
    path: &str,
    path_item: &'a Value,
    method_name: &str,
    operation: &'a Value,
    method: HttpMethod,
    used_keys: &mut HashSet<String>,
    budget: &mut SchemaBudget,
) -> Result<ParsedOperation, OpenApiImportError> {
    let operation_id = operation
        .get("operationId")
        .and_then(Value::as_str)
        .map(str::to_string);

    let base_key = operation_id
        .as_deref()
        .map(slugify)
        .filter(|key| !key.is_empty())
        .unwrap_or_else(|| slugify(&format!("{method_name}-{path}")));
    let skill_key = dedupe_key(&base_key, used_keys);

    let display_name = operation
        .get("summary")
        .and_then(Value::as_str)
        .filter(|summary| !summary.trim().is_empty())
        .or(operation_id.as_deref())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{} {}", method_name.to_uppercase(), path));
    let display_name = truncate_chars(display_name.trim(), MAX_DISPLAY_NAME_LEN);

    let description = operation
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string);

    let tags = operation
        .get("tags")
        .and_then(Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let params_schema = build_params_schema(root, path_item, operation, &skill_key, budget)?;

    Ok(ParsedOperation {
        skill_key,
        display_name,
        description,
        tags,
        params_schema,
        method,
        path: path.to_string(),
    })
}

/// Flattens `parameters` and `requestBody` into one JSON-Schema object — see the module
/// docs for exactly what is and is not merged.
///
/// Every value this copies into `properties` is charged against `budget` *before* the copy,
/// because the copies are the whole amplification: a resolved `$ref` is a deep clone of a
/// subtree the document holds once and this function may hold three hundred times.
fn build_params_schema<'a>(
    root: &'a Value,
    path_item: &'a Value,
    operation: &'a Value,
    skill_key: &str,
    budget: &mut SchemaBudget,
) -> Result<Value, OpenApiImportError> {
    let mut properties = Map::new();
    let mut required: Vec<String> = Vec::new();
    let mut body_property: Option<String> = None;
    budget.begin_operation();

    let mut param_list: Vec<(&'a Value, &'a str, Option<&'a str>)> = Vec::new();

    let mut process_param_array = |params: &'a Value| {
        if let Some(arr) = params.as_array() {
            for param in arr {
                let resolved = resolve_maybe_ref(root, param);
                if let Some(name) = resolved.get("name").and_then(Value::as_str) {
                    let param_in = resolved.get("in").and_then(Value::as_str);
                    if let Some(pos) = param_list
                        .iter()
                        .position(|(_, n, i)| *n == name && *i == param_in)
                    {
                        param_list[pos] = (resolved, name, param_in);
                    } else {
                        param_list.push((resolved, name, param_in));
                    }
                }
            }
        }
    };

    if let Some(params) = path_item.get("parameters") {
        process_param_array(params);
    }
    if let Some(params) = operation.get("parameters") {
        process_param_array(params);
    }

    for (parameter, name, _) in param_list {
        let declared = parameter.get("schema");
        budget.charge(declared.map_or(0, serialized_len), skill_key)?;
        let schema = declared
            .cloned()
            .unwrap_or_else(|| json!({"type": "string"}));
        if parameter
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            required.push(name.to_string());
        }
        properties.insert(name.to_string(), schema);
    }

    if let Some(request_body) = operation.get("requestBody") {
        let request_body = resolve_maybe_ref(root, request_body);
        if let Some(schema) = request_body
            .get("content")
            .and_then(Value::as_object)
            .and_then(|content| content.get("application/json"))
            .and_then(|media_type| media_type.get("schema"))
        {
            let schema = resolve_maybe_ref(root, schema);
            budget.charge(serialized_len(schema), skill_key)?;
            let chosen = free_body_property_name(&properties);
            properties.insert(chosen.clone(), schema.clone());
            if request_body
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                required.push(chosen.clone());
            }
            body_property = Some(chosen);
        }
    }

    let mut unique_required = Vec::new();
    for req in required {
        if !unique_required.contains(&req) {
            unique_required.push(req);
        }
    }

    let mut schema = Map::new();
    schema.insert("type".to_string(), json!("object"));
    schema.insert("properties".to_string(), Value::Object(properties));
    if !unique_required.is_empty() {
        schema.insert("required".to_string(), json!(unique_required));
    }
    // Only when the name is not the one `body_property_name` already assumes — see
    // `BODY_PROPERTY_ANNOTATION` for why the ordinary schema is left exactly as it was.
    if let Some(body_property) = body_property
        && body_property != DEFAULT_BODY_PROPERTY
    {
        schema.insert(BODY_PROPERTY_ANNOTATION.to_string(), json!(body_property));
    }
    Ok(Value::Object(schema))
}

/// Resolves a `{"$ref": "#/..."}` object exactly one level against `root`. Anything that is
/// not a `$ref` object — including an already-resolved schema, or an external `$ref` this
/// module does not follow — is returned unchanged.
fn resolve_maybe_ref<'a>(root: &'a Value, value: &'a Value) -> &'a Value {
    match value.get("$ref").and_then(Value::as_str) {
        Some(pointer) => resolve_json_pointer(root, pointer).unwrap_or(value),
        None => value,
    }
}

/// A minimal local (`#/...`) JSON Pointer (RFC 6901) resolver. Returns `None` for an
/// external reference or a pointer that does not resolve within `root`.
fn resolve_json_pointer<'a>(root: &'a Value, pointer: &str) -> Option<&'a Value> {
    let path = pointer.strip_prefix("#/")?;
    let mut current = root;
    for raw_segment in path.split('/') {
        let segment = raw_segment.replace("~1", "/").replace("~0", "~");
        current = match current {
            Value::Object(map) => map.get(&segment)?,
            Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

/// Lowercases, maps every byte outside `[a-z0-9_-]` to `-`, collapses consecutive `-` (the
/// fallback `{method}-{path}` key deliberately joins with `-` and `path` always starts with
/// `/` — itself mapped to `-` — so an uncollapsed run would otherwise double up there), then
/// trims leading/trailing non-alphanumerics so the result satisfies `skills_skill_key_valid`
/// (`^[a-z0-9]([a-z0-9_-]*[a-z0-9])?$`) whenever the input contains at least one
/// alphanumeric character.
fn slugify(input: &str) -> String {
    let mapped: String = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else if ch == '_' || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let collapsed = collapse_consecutive_dashes(&mapped);
    let trimmed = collapsed
        .trim_matches(|ch: char| !ch.is_ascii_alphanumeric())
        .to_string();
    truncate_slug(&trimmed, MAX_SKILL_KEY_LEN)
}

fn collapse_consecutive_dashes(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut last_was_dash = false;
    for ch in input.chars() {
        if ch == '-' {
            if !last_was_dash {
                result.push(ch);
            }
            last_was_dash = true;
        } else {
            result.push(ch);
            last_was_dash = false;
        }
    }
    result
}

/// Truncates to at most `max_len` **characters** (not bytes) and re-trims a trailing
/// non-alphanumeric the cut may have exposed, so the invariant `slugify` establishes
/// survives truncation too.
fn truncate_slug(input: &str, max_len: usize) -> String {
    let truncated: String = input.chars().take(max_len).collect();
    truncated
        .trim_end_matches(|ch: char| !ch.is_ascii_alphanumeric())
        .to_string()
}

fn truncate_chars(input: &str, max_len: usize) -> String {
    input.chars().take(max_len).collect()
}

/// Makes `base` unique against `used`, appending `-2`, `-3`, … on collision. Reserves room
/// for the suffix by truncating `base` first, so the final key never exceeds
/// [`MAX_SKILL_KEY_LEN`] plus a couple of digits — comfortably under the 128-character
/// database constraint.
fn dedupe_key(base: &str, used: &mut HashSet<String>) -> String {
    let base = if base.is_empty() {
        "operation".to_string()
    } else {
        base.to_string()
    };
    if used.insert(base.clone()) {
        return base;
    }
    for suffix in 2..=MAX_IMPORT_OPERATIONS + 1 {
        let candidate = format!("{base}-{suffix}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    // Unreachable given the caller enforces `MAX_IMPORT_OPERATIONS`, which bounds how many
    // times any one base key could possibly collide — but a bare `base` string with no
    // numeric suffix could never collide with `"{base}-{suffix}"` for the loop above, so a
    // fallback that cannot itself collide closes the branch without ever running in
    // practice.
    format!("{base}-{}", used.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_document(paths: Value) -> Value {
        json!({
            "openapi": "3.0.3",
            "info": {"title": "Test API", "version": "1.0.0"},
            "servers": [{"url": "https://api.example.com/v1"}],
            "paths": paths,
        })
    }

    #[test]
    fn parses_a_single_get_operation_with_a_path_parameter() {
        let document = minimal_document(json!({
            "/orders/{order_id}": {
                "get": {
                    "operationId": "getOrder",
                    "summary": "Get an order",
                    "tags": ["orders"],
                    "parameters": [
                        {"name": "order_id", "in": "path", "required": true, "schema": {"type": "string"}}
                    ]
                }
            }
        }));

        let parsed = parse_openapi_document(&document).expect("must parse");
        assert_eq!(parsed.base_url, "https://api.example.com/v1");
        assert_eq!(parsed.operations.len(), 1);

        let op = &parsed.operations[0];
        assert_eq!(op.skill_key, "getorder");
        assert_eq!(op.display_name, "Get an order");
        assert_eq!(op.tags, vec!["orders".to_string()]);
        assert_eq!(op.method, HttpMethod::Get);
        assert_eq!(op.path, "/orders/{order_id}");
        assert_eq!(op.params_schema["type"], "object");
        assert_eq!(op.params_schema["properties"]["order_id"]["type"], "string");
        assert_eq!(op.params_schema["required"], json!(["order_id"]));
    }

    #[test]
    fn falls_back_to_method_and_path_when_operation_id_is_absent() {
        let document = minimal_document(json!({
            "/widgets": {
                "post": {}
            }
        }));

        let parsed = parse_openapi_document(&document).expect("must parse");
        let op = &parsed.operations[0];
        assert_eq!(op.skill_key, "post-widgets");
        assert_eq!(op.display_name, "POST /widgets");
    }

    #[test]
    fn deduplicates_colliding_skill_keys_within_one_import() {
        let document = minimal_document(json!({
            "/a": {"get": {"operationId": "dup"}},
            "/b": {"get": {"operationId": "dup"}},
            "/c": {"get": {"operationId": "dup"}},
        }));

        let parsed = parse_openapi_document(&document).expect("must parse");
        let keys: Vec<&str> = parsed
            .operations
            .iter()
            .map(|op| op.skill_key.as_str())
            .collect();
        assert_eq!(keys, vec!["dup", "dup-2", "dup-3"]);
    }

    #[test]
    fn resolves_a_component_request_body_schema_one_level() {
        let document = json!({
            "openapi": "3.1.0",
            "servers": [{"url": "https://api.example.com"}],
            "paths": {
                "/orders": {
                    "post": {
                        "operationId": "createOrder",
                        "requestBody": {
                            "required": true,
                            "content": {
                                "application/json": {
                                    "schema": {"$ref": "#/components/schemas/CreateOrder"}
                                }
                            }
                        }
                    }
                }
            },
            "components": {
                "schemas": {
                    "CreateOrder": {
                        "type": "object",
                        "properties": {"sku": {"type": "string"}}
                    }
                }
            }
        });

        let parsed = parse_openapi_document(&document).expect("must parse");
        let op = &parsed.operations[0];
        assert_eq!(op.params_schema["required"], json!(["body"]));
        assert_eq!(
            op.params_schema["properties"]["body"]["properties"]["sku"]["type"],
            "string"
        );
        // The uncontested case must stay exactly the schema it always was: no annotation, so
        // nothing new is put in front of a provider, and `body_property_name` still answers.
        assert!(op.params_schema.get(BODY_PROPERTY_ANNOTATION).is_none());
        assert_eq!(body_property_name(&op.params_schema), Some("body"));
    }

    #[test]
    fn resolves_a_referenced_parameter_one_level() {
        let document = json!({
            "openapi": "3.0.3",
            "servers": [{"url": "https://api.example.com"}],
            "paths": {
                "/widgets": {
                    "get": {
                        "operationId": "listWidgets",
                        "parameters": [{"$ref": "#/components/parameters/PageSize"}]
                    }
                }
            },
            "components": {
                "parameters": {
                    "PageSize": {
                        "name": "page_size",
                        "in": "query",
                        "required": false,
                        "schema": {"type": "integer"}
                    }
                }
            }
        });

        let parsed = parse_openapi_document(&document).expect("must parse");
        let op = &parsed.operations[0];
        assert_eq!(
            op.params_schema["properties"]["page_size"]["type"],
            "integer"
        );
        assert!(op.params_schema.get("required").is_none());
    }

    #[test]
    fn skips_methods_the_executor_table_cannot_store() {
        let document = minimal_document(json!({
            "/widgets": {
                "get": {"operationId": "getWidget"},
                "options": {"operationId": "optionsWidget"},
                "head": {"operationId": "headWidget"},
                "trace": {"operationId": "traceWidget"},
                "parameters": [{"name": "shared", "in": "query", "schema": {"type": "string"}}]
            }
        }));

        let parsed = parse_openapi_document(&document).expect("must parse");
        assert_eq!(parsed.operations.len(), 1);
        assert_eq!(parsed.operations[0].method, HttpMethod::Get);
    }

    #[test]
    fn rejects_a_document_that_is_not_an_object() {
        let error = parse_openapi_document(&json!(["not", "an", "object"])).unwrap_err();
        assert_eq!(error, OpenApiImportError::NotAnObject);
    }

    #[test]
    fn rejects_swagger_2_0_as_an_unsupported_version() {
        let document = json!({
            "swagger": "2.0",
            "paths": {"/x": {"get": {}}}
        });
        let error = parse_openapi_document(&document).unwrap_err();
        assert_eq!(error, OpenApiImportError::UnsupportedVersion);
    }

    #[test]
    fn rejects_a_document_with_no_server_url() {
        let document = json!({
            "openapi": "3.0.3",
            "paths": {"/x": {"get": {}}}
        });
        let error = parse_openapi_document(&document).unwrap_err();
        assert_eq!(error, OpenApiImportError::MissingServerUrl);
    }

    #[test]
    fn rejects_an_invalid_server_url() {
        let document = json!({
            "openapi": "3.0.3",
            "servers": [{"url": "not a url"}],
            "paths": {"/x": {"get": {}}}
        });
        let error = parse_openapi_document(&document).unwrap_err();
        assert!(matches!(error, OpenApiImportError::InvalidServerUrl(_)));
    }

    #[test]
    fn rejects_a_document_with_no_operations() {
        let document = minimal_document(json!({}));
        let error = parse_openapi_document(&document).unwrap_err();
        assert_eq!(error, OpenApiImportError::NoOperations);
    }

    /// The cap is enforced on the *count*, never a silent truncation — a document with 301
    /// operations must be refused outright, and the refusal must carry the true count.
    #[test]
    fn rejects_a_document_over_the_operation_cap_without_truncating() {
        let mut paths = Map::new();
        for index in 0..(MAX_IMPORT_OPERATIONS + 1) {
            paths.insert(
                format!("/op{index}"),
                json!({"get": {"operationId": format!("op{index}")}}),
            );
        }
        let document = minimal_document(Value::Object(paths));

        let error = parse_openapi_document(&document).unwrap_err();
        assert_eq!(
            error,
            OpenApiImportError::TooManyOperations {
                found: MAX_IMPORT_OPERATIONS + 1,
                cap: MAX_IMPORT_OPERATIONS,
            }
        );
    }

    /// The cap boundary itself must still succeed — this is a "more than", not "at least",
    /// cap.
    #[test]
    fn accepts_a_document_exactly_at_the_operation_cap() {
        let mut paths = Map::new();
        for index in 0..MAX_IMPORT_OPERATIONS {
            paths.insert(
                format!("/op{index}"),
                json!({"get": {"operationId": format!("op{index}")}}),
            );
        }
        let document = minimal_document(Value::Object(paths));

        let parsed = parse_openapi_document(&document).expect("must parse at the cap");
        assert_eq!(parsed.operations.len(), MAX_IMPORT_OPERATIONS);
    }

    // -----------------------------------------------------------------------------------
    // The byte budget
    // -----------------------------------------------------------------------------------

    /// The amplification the operation cap could not see: one big component schema, three
    /// hundred tiny operations that each `$ref` it. Every dimension the old guard measured is
    /// inside its limit — 300 operations exactly at the cap, a document well under the admin
    /// body limit — and the derived total is what explodes.
    ///
    /// Deliberately built at the shape of the real attack rather than with one absurd schema,
    /// because a per-operation cap alone passes this and the aggregate cap is the fix.
    ///
    /// Sized by property *count* rather than by a target byte size, so the fixture cannot
    /// silently drift away from what it claims; every test that uses it measures the result
    /// with `serialized_len` and asserts which side of which cap it landed on.
    fn ref_amplification_document(operations: usize, component_properties: usize) -> Value {
        let mut properties = Map::new();
        for index in 0..component_properties {
            properties.insert(
                format!("field_{index:06}"),
                json!({"type": "string", "description": "x"}),
            );
        }
        let mut paths = Map::new();
        for index in 0..operations {
            paths.insert(
                format!("/op{index}"),
                json!({
                    "post": {
                        "operationId": format!("op{index}"),
                        "requestBody": {
                            "content": {
                                "application/json": {
                                    "schema": {"$ref": "#/components/schemas/Big"}
                                }
                            }
                        }
                    }
                }),
            );
        }
        json!({
            "openapi": "3.0.3",
            "servers": [{"url": "https://api.example.com"}],
            "paths": Value::Object(paths),
            "components": {"schemas": {"Big": {"type": "object", "properties": properties}}},
        })
    }

    /// The serialised size of the `Big` component `ref_amplification_document` builds — the
    /// exact number each operation's `params_schema` would carry a copy of.
    fn referenced_component_bytes(document: &Value) -> usize {
        serialized_len(&document["components"]["schemas"]["Big"])
    }

    #[test]
    fn rejects_ref_amplification_that_the_operation_cap_cannot_see() {
        let document = ref_amplification_document(MAX_IMPORT_OPERATIONS, 800);

        // The fixture must be inside every guard that existed before this change, or it
        // proves the wrong thing: at the operation cap, under the document byte cap, and with
        // a per-operation schema that the per-operation cap would happily accept.
        assert!(
            serialized_len(&document) <= MAX_DOCUMENT_BYTES,
            "the fixture must pass the document cap"
        );
        let per_operation = referenced_component_bytes(&document);
        assert!(
            per_operation < MAX_OPERATION_SCHEMA_BYTES,
            "each operation's schema must be individually acceptable ({per_operation} bytes)"
        );
        assert!(
            per_operation * MAX_IMPORT_OPERATIONS > MAX_TOTAL_SCHEMA_BYTES,
            "the fixture must amplify past the aggregate cap, or it proves nothing"
        );

        let error = parse_openapi_document(&document).unwrap_err();
        let OpenApiImportError::TotalSchemaTooLarge { bytes, cap } = error else {
            panic!("expected the aggregate schema budget to refuse this, got {error:?}");
        };
        assert_eq!(cap, MAX_TOTAL_SCHEMA_BYTES);
        // The refusal must come from the running total, not from one operation: proof the
        // budget is charged across operations rather than reset for each.
        assert!(bytes > MAX_OPERATION_SCHEMA_BYTES);
    }

    /// The aggregate cap must not be reachable only by many operations — one operation whose
    /// own resolved schema is absurd is refused on its own, and named.
    #[test]
    fn rejects_one_operation_whose_resolved_schema_is_over_the_per_operation_cap() {
        let document = ref_amplification_document(1, 2_000);
        assert!(referenced_component_bytes(&document) > MAX_OPERATION_SCHEMA_BYTES);
        assert!(serialized_len(&document) <= MAX_DOCUMENT_BYTES);

        let error = parse_openapi_document(&document).unwrap_err();
        let OpenApiImportError::OperationSchemaTooLarge {
            skill_key,
            bytes,
            cap,
        } = error
        else {
            panic!("expected the per-operation schema cap to refuse this, got {error:?}");
        };
        assert_eq!(skill_key, "op0", "the refusal must name the operation");
        assert_eq!(cap, MAX_OPERATION_SCHEMA_BYTES);
        assert!(bytes > MAX_OPERATION_SCHEMA_BYTES);
    }

    /// `{"$ref": "#/paths"}` reaches the same amplification with no oversized component at
    /// all — the self-reference resolves to the whole `paths` object, once per operation.
    #[test]
    fn rejects_a_self_referential_ref_that_resolves_to_the_whole_paths_object() {
        let mut paths = Map::new();
        for index in 0..MAX_IMPORT_OPERATIONS {
            paths.insert(
                format!("/op{index}"),
                json!({
                    "post": {
                        "operationId": format!("op{index}"),
                        "description": "y".repeat(300),
                        "requestBody": {
                            "content": {"application/json": {"schema": {"$ref": "#/paths"}}}
                        }
                    }
                }),
            );
        }
        let document = json!({
            "openapi": "3.0.3",
            "servers": [{"url": "https://api.example.com"}],
            "paths": Value::Object(paths),
        });
        assert!(serialized_len(&document) <= MAX_DOCUMENT_BYTES);

        let error = parse_openapi_document(&document).unwrap_err();
        assert!(
            matches!(
                error,
                OpenApiImportError::OperationSchemaTooLarge { .. }
                    | OpenApiImportError::TotalSchemaTooLarge { .. }
            ),
            "a self-referential $ref must hit the byte budget, got {error:?}"
        );
    }

    #[test]
    fn rejects_a_document_larger_than_the_byte_cap_before_deriving_anything() {
        let mut paths = Map::new();
        for index in 0..10 {
            paths.insert(
                format!("/op{index}"),
                json!({"get": {
                    "operationId": format!("op{index}"),
                    "description": "z".repeat(MAX_DOCUMENT_BYTES / 8),
                }}),
            );
        }
        let document = minimal_document(Value::Object(paths));

        let error = parse_openapi_document(&document).unwrap_err();
        let OpenApiImportError::DocumentTooLarge { bytes, cap } = error else {
            panic!("expected the document byte cap to refuse this, got {error:?}");
        };
        assert_eq!(cap, MAX_DOCUMENT_BYTES);
        assert!(bytes > MAX_DOCUMENT_BYTES);
    }

    /// The guard must be invisible to an ordinary spec: a document that is merely detailed
    /// still imports, and its schemas are still inlined.
    #[test]
    fn an_ordinary_document_is_unaffected_by_the_byte_budget() {
        let document = ref_amplification_document(50, 50);
        let parsed = parse_openapi_document(&document).expect("an ordinary document must parse");
        assert_eq!(parsed.operations.len(), 50);
        assert!(
            parsed.operations[0].params_schema["properties"]["body"]["properties"]["field_000000"]
                .is_object(),
            "the referenced schema must still be resolved and inlined"
        );
    }

    /// `serialized_len` is the measurement the whole budget rests on; if it disagreed with
    /// `to_string().len()` the caps would be enforced against a number nobody can reproduce.
    #[test]
    fn serialized_len_matches_the_string_encoding_it_stands_in_for() {
        for value in [
            json!(null),
            json!({"a": [1, 2, 3], "b": {"c": "déjà vu"}}),
            json!("a string with \"quotes\" and \\ escapes"),
        ] {
            assert_eq!(serialized_len(&value), value.to_string().len());
        }
    }

    #[test]
    fn slugify_produces_keys_matching_the_skill_key_constraint() {
        for (input, expected) in [
            ("Get Order!!", "get-order"),
            ("__weird__", "weird"),
            ("already-valid_key", "already-valid_key"),
            ("123", "123"),
            ("", ""),
        ] {
            assert_eq!(slugify(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn skill_key_is_truncated_to_the_database_constraint() {
        let long_operation_id = "a".repeat(500);
        let document = minimal_document(json!({
            "/x": {"get": {"operationId": long_operation_id}}
        }));
        let parsed = parse_openapi_document(&document).expect("must parse");
        assert!(parsed.operations[0].skill_key.len() <= MAX_SKILL_KEY_LEN);
    }

    #[test]
    fn path_item_level_parameters_are_inherited_and_overridden_by_operation_level() {
        let document = minimal_document(json!({
            "/orders/{order_id}": {
                "parameters": [
                    {"name": "order_id", "in": "path", "required": true, "schema": {"type": "string"}},
                    {"name": "tenant", "in": "header", "schema": {"type": "string"}}
                ],
                "get": {
                    "operationId": "getOrder",
                    "parameters": [
                        {"name": "order_id", "in": "path", "required": true, "schema": {"type": "string", "format": "uuid"}}
                    ]
                }
            }
        }));

        let parsed = parse_openapi_document(&document).expect("must parse");
        let op = &parsed.operations[0];
        let props = op.params_schema["properties"].as_object().unwrap();

        assert!(props.contains_key("order_id"));
        assert!(props.contains_key("tenant"));
        assert_eq!(props["order_id"]["format"], "uuid");
    }

    #[test]
    fn body_parameter_and_request_body_collision_is_prevented_and_required_is_deduplicated() {
        let document = minimal_document(json!({
            "/items": {
                "post": {
                    "operationId": "createItem",
                    "parameters": [
                        {"name": "body", "in": "query", "required": true, "schema": {"type": "string"}},
                        {"name": "body", "in": "header", "required": true, "schema": {"type": "string"}}
                    ],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {"type": "object", "properties": {"name": {"type": "string"}}}
                            }
                        }
                    }
                }
            }
        }));

        let parsed = parse_openapi_document(&document).expect("must parse");
        let op = &parsed.operations[0];
        let props = op.params_schema["properties"].as_object().unwrap();

        assert!(props.contains_key("body"));
        assert!(props.contains_key("request_body"));

        let req = op.params_schema["required"].as_array().unwrap();
        let req_strings: Vec<&str> = req.iter().filter_map(Value::as_str).collect();
        assert_eq!(req_strings, vec!["body", "request_body"]);

        // Renaming the property is only half the job — the schema has to say which property
        // it became, or the executor cannot know. `skill_tool` reads exactly this.
        assert_eq!(
            body_property_name(&op.params_schema),
            Some("request_body"),
            "the renamed body must be discoverable, not merely present"
        );
    }

    /// `request_body` was the whole fallback, so a document that also declares a parameter
    /// by that name put three values into two properties and lost one without a word.
    #[test]
    fn a_parameter_named_request_body_does_not_take_the_fallback_from_the_body() {
        let document = minimal_document(json!({
            "/items": {
                "post": {
                    "operationId": "createItem",
                    "parameters": [
                        {"name": "body", "in": "query", "required": true, "schema": {"type": "string"}},
                        {"name": "request_body", "in": "query", "required": true, "schema": {"type": "integer"}}
                    ],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {"type": "object", "properties": {"name": {"type": "string"}}}
                            }
                        }
                    }
                }
            }
        }));

        let parsed = parse_openapi_document(&document).expect("must parse");
        let op = &parsed.operations[0];
        let props = op.params_schema["properties"].as_object().unwrap();

        assert_eq!(
            props["body"]["type"], "string",
            "the parameter keeps `body`"
        );
        assert_eq!(
            props["request_body"]["type"], "integer",
            "the parameter keeps `request_body`"
        );
        assert_eq!(
            props["request_body_2"]["properties"]["name"]["type"], "string",
            "the request body takes the first name neither parameter claimed"
        );

        let req = op.params_schema["required"].as_array().unwrap();
        let req_strings: Vec<&str> = req.iter().filter_map(Value::as_str).collect();
        assert_eq!(req_strings, vec!["body", "request_body", "request_body_2"]);
        assert_eq!(
            body_property_name(&op.params_schema),
            Some("request_body_2")
        );
    }

    /// The annotation is a hint, not an authority: it can only ever name a property that is
    /// really there, so a hand-edited `params_schema` cannot point the executor at nothing.
    #[test]
    fn an_annotation_naming_a_property_that_does_not_exist_is_ignored() {
        assert_eq!(
            body_property_name(&json!({
                "type": "object",
                "properties": {"body": {"type": "object"}, "page": {"type": "integer"}},
                BODY_PROPERTY_ANNOTATION: "not_a_property"
            })),
            Some("body")
        );
        assert_eq!(
            body_property_name(&json!({
                "type": "object",
                "properties": {"page": {"type": "integer"}},
                BODY_PROPERTY_ANNOTATION: "not_a_property"
            })),
            None
        );
        assert_eq!(
            body_property_name(&json!({"type": "object", "properties": {}})),
            None
        );
    }
}
