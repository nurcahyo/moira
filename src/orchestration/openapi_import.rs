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
//!   a `body` property. This is a deliberate simplification, not a full OpenAPI-to-
//!   JSON-Schema translator: a parameter and the request body can never collide (`body`
//!   is not itself a legal OpenAPI parameter name in this shape), but two parameters that
//!   share a name across different `in` locations (e.g. a path param and a query param
//!   both named `id`) do collide, and the later one silently wins.
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

use std::{collections::HashSet, fmt};

use serde_json::{Map, Value, json};

use crate::domain::HttpMethod;

/// Largest number of operations a single import accepts (§5 decision 23). A spec over this
/// is rejected outright — never silently truncated — so the operator learns the true
/// operation count and can split the import rather than discover a partial one later.
pub const MAX_IMPORT_OPERATIONS: usize = 300;

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
/// [`MAX_IMPORT_OPERATIONS`] but performs no SSRF check and no I/O — see the module docs.
pub fn parse_openapi_document(document: &Value) -> Result<ParsedImport, OpenApiImportError> {
    let root = document
        .as_object()
        .ok_or(OpenApiImportError::NotAnObject)?;

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
    let mut raw_operations: Vec<(&str, &str, &Value, HttpMethod)> = Vec::new();
    for path in path_keys {
        let Some(item) = paths.get(path).and_then(Value::as_object) else {
            continue;
        };
        for (method_name, method) in RECOGNIZED_METHODS {
            if let Some(operation) = item.get(method_name)
                && operation.is_object()
            {
                raw_operations.push((path.as_str(), method_name, operation, method));
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
    let operations = raw_operations
        .into_iter()
        .map(|(path, method_name, operation, method)| {
            build_operation(
                document,
                path,
                method_name,
                operation,
                method,
                &mut used_keys,
            )
        })
        .collect();

    Ok(ParsedImport {
        base_url,
        operations,
    })
}

fn build_operation(
    root: &Value,
    path: &str,
    method_name: &str,
    operation: &Value,
    method: HttpMethod,
    used_keys: &mut HashSet<String>,
) -> ParsedOperation {
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

    let params_schema = build_params_schema(root, operation);

    ParsedOperation {
        skill_key,
        display_name,
        description,
        tags,
        params_schema,
        method,
        path: path.to_string(),
    }
}

/// Flattens `parameters` and `requestBody` into one JSON-Schema object — see the module
/// docs for exactly what is and is not merged.
fn build_params_schema(root: &Value, operation: &Value) -> Value {
    let mut properties = Map::new();
    let mut required: Vec<String> = Vec::new();

    if let Some(parameters) = operation.get("parameters").and_then(Value::as_array) {
        for parameter in parameters {
            let parameter = resolve_maybe_ref(root, parameter);
            let Some(name) = parameter.get("name").and_then(Value::as_str) else {
                continue;
            };
            let schema = parameter
                .get("schema")
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
            properties.insert("body".to_string(), schema.clone());
            if request_body
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                required.push("body".to_string());
            }
        }
    }

    let mut schema = Map::new();
    schema.insert("type".to_string(), json!("object"));
    schema.insert("properties".to_string(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".to_string(), json!(required));
    }
    Value::Object(schema)
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
}
