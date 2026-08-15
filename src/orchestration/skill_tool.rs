//! `HttpSkillTool` and the multi-turn tool loop — the execution half of plan 12 §5
//! (issue #84, partial: the flow engine is a separate stage).
//!
//! This is where an enabled `skills` row (`kind = 'tool'`) plus its `skill_http_executors`
//! row becomes a live `rig_core::tool::Tool` a model can actually call. Everything Rig
//! owns stays here per `.agents/skills/moira-rig-integration/SKILL.md`: `Tool`, `ToolSet`,
//! `ToolDefinition`, `ToolCall` and the `CompletionRequest` the loop re-issues never
//! escape `src/orchestration`.
//!
//! # What Moira keeps
//!
//! Rig has no per-tool timeout (`ToolSet` and `ToolServer` never bound a call; only MCP
//! tools get one, and `rmcp` is off), no notion of an allowed host, and no opinion about
//! credentials. So this module owns all three:
//!
//! * **Execution-time SSRF** (plan 12 risk R20). `url_template` carries `{placeholder}`
//!   segments, so the final URL only exists at call time — import-time validation of the
//!   server URL cannot cover it. Every call re-validates the *resolved* URL through the
//!   same `security::ssrf::validate_outbound_url` guard and additionally requires the host
//!   to equal the stored `allowed_host`, so no argument the model invents can move the
//!   request to another origin.
//! * **The credential** is decrypted by the caller and handed over as a `SecretString`. It
//!   is never a tool argument (a model authors those), never in `Output`, never in a
//!   `Debug` rendering, and never logged.
//! * **A bounded timeout and a bounded response**, because a skill target is a third-party
//!   endpoint whose latency and body size Moira does not control.
//!
//! # Failure posture
//!
//! In-band by default, per `.agents/skills/moira-rig-tools/SKILL.md`: a refusal or an
//! upstream error becomes a classified `ToolFailure` whose `model_output` the model sees
//! and can recover from within the same execution. Only conditions that make the whole
//! attempt impossible (an exhausted turn budget, cancellation) become an `ExecutionFailure`.

use std::{collections::HashSet, time::Duration};

use rig_core::{
    OneOrMany,
    completion::{
        CompletionRequest, ToolDefinition,
        message::{AssistantContent, Message, ToolCall, ToolResultContent, UserContent},
    },
    tool::{Tool, ToolCallExtensions, ToolFailure, ToolSet},
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use url::Url;
use uuid::Uuid;

use crate::{
    domain::{
        CredentialType, ExecutionFailure, ExecutionFailureClass, GuardContext, GuardVerdict,
        HttpMethod, SkillGuard, evaluate_guards,
    },
    orchestration::RuntimeModelHandle,
    security::{OutboundUrlPolicy, SystemResolver, validate_outbound_url},
};

/// Per-call caller context Moira injects through `ToolCallExtensions`. Never serialized to
/// the model, and never populated from tool arguments — a model can name any tenant it
/// likes, so scoping must come from Moira's own resolution.
#[derive(Clone, Debug)]
pub struct SkillCallerScope {
    pub request_id: String,
    pub execution_id: Uuid,
    pub external_tenant_id: Option<String>,
    pub application_id: Option<Uuid>,
}

/// The address-space policy applied to a skill call's **resolved** URL.
#[derive(Debug, Clone)]
pub struct SkillOutboundPolicy {
    pub dns_timeout: Duration,
    /// Dev-only, mirrors `settings.skill_execution.allow_insecure_dev_urls`. Production
    /// start-up refuses to come up while it is true (`Settings::validate`).
    pub allow_insecure: bool,
}

/// Everything one skill needs to become a callable tool. Assembled in
/// `application::execution` from a `skills` row, its `skill_http_executors` row, and the
/// decrypted credential that row references.
pub struct SkillToolSpec {
    pub skill_key: String,
    pub description: String,
    pub params_schema: Value,
    pub method: HttpMethod,
    pub url_template: String,
    pub allowed_host: String,
    pub header_template: Value,
    pub credential: Option<SkillCredential>,
    pub timeout: Duration,
    pub maximum_response_bytes: usize,
    pub outbound_policy: SkillOutboundPolicy,
}

impl std::fmt::Debug for SkillToolSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillToolSpec")
            .field("skill_key", &self.skill_key)
            .field("method", &self.method)
            .field("url_template", &self.url_template)
            .field("allowed_host", &self.allowed_host)
            .field("credential", &self.credential.is_some())
            .finish_non_exhaustive()
    }
}

/// The decrypted secret a skill call authenticates with.
///
/// Manual `Debug` (never derived) for the same reason `RuntimeModelHandle` and
/// `ResolvedCredential` have one: this type is reachable from a struct a panic or a
/// `tracing` field could render.
pub struct SkillCredential {
    pub credential_type: CredentialType,
    pub secret: SecretString,
}

impl std::fmt::Debug for SkillCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SkillCredential({:?}, <redacted>)", self.credential_type)
    }
}

/// Why a skill could not be turned into a callable tool at all — a configuration fault,
/// decided before any model sees the tool, never a per-call failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillToolBuildError {
    /// Two `skill_refs` entries advertise the same `skill_key`. `ToolSet` would replace
    /// one in place with only a `warn!`, so the collision is refused here instead.
    DuplicateSkillKey,
    /// `params_schema` is not a JSON-Schema object, or declares a `required` name that is
    /// not in `properties`. Providers reject a non-object parameter schema outright.
    InvalidParametersSchema,
    /// `header_template` is not an object of string values.
    InvalidHeaderTemplate,
    /// The credential type cannot be turned into an HTTP authorization header. Fail-closed:
    /// the alternative is calling the target unauthenticated.
    UnsupportedCredentialType,
}

impl SkillToolBuildError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DuplicateSkillKey => "duplicate_skill_key",
            Self::InvalidParametersSchema => "invalid_parameters_schema",
            Self::InvalidHeaderTemplate => "invalid_header_template",
            Self::UnsupportedCredentialType => "unsupported_credential_type",
        }
    }
}

/// A skill call's failure modes. Lowercase messages without a trailing period, Moira style.
///
/// Every message here is model-visible through `ToolExecutionResult::model_output`, so none
/// of them names a host, an internal address, a header value or a response body — the
/// sanitisation contract `safe_provider_error_message` already applies at the Rig boundary.
#[derive(Debug, thiserror::Error)]
pub enum SkillToolError {
    #[error("tool arguments did not match the skill's parameter schema")]
    InvalidArguments,
    #[error("the skill's target url could not be resolved from its template")]
    UrlTemplate,
    #[error("the skill's target address is not permitted")]
    AddressNotPermitted,
    #[error("the skill call exceeded its timeout")]
    Timeout,
    #[error("the skill target could not be reached")]
    Transport,
    #[error("the skill target answered http {status}")]
    Upstream { status: u16 },
    #[error("the skill target's response could not be read")]
    ResponseUnreadable,
}

/// A skill call's result, as the model sees it.
///
/// A real struct rather than `type Output = String`: `serialize_tool_output` passes a
/// `String` through verbatim, which makes a JSON-shaped string indistinguishable from a
/// structured result on the wire. Never carries a header, a credential, or anything but
/// the target's own status and body.
#[derive(Debug, Serialize)]
pub struct SkillCallOutput {
    pub status: u16,
    /// The response body, parsed as JSON when the target sent JSON and carried as text
    /// otherwise. Bounded by `maximum_response_bytes`.
    pub body: Value,
    /// `true` when the body was cut at the byte ceiling, so the model is told the answer
    /// is partial instead of silently reasoning over a truncated document.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// Tool arguments are whatever the model sent. Validated against `params_schema` inside
/// `call_with_extensions` rather than by serde, because the schema is per-instance data.
#[derive(Debug, Deserialize)]
pub struct SkillToolArgs(pub Value);

/// One enabled skill, live.
pub struct HttpSkillTool {
    spec: SkillToolSpec,
    client: reqwest::Client,
}

impl std::fmt::Debug for HttpSkillTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HttpSkillTool({}, <redacted>)", self.spec.skill_key)
    }
}

impl HttpSkillTool {
    pub fn new(spec: SkillToolSpec, client: reqwest::Client) -> Result<Self, SkillToolBuildError> {
        validate_parameters_schema(&spec.params_schema)?;
        header_pairs(&spec.header_template)?;
        if let Some(credential) = &spec.credential {
            authorization_header(credential)?;
        }
        Ok(Self { spec, client })
    }

    pub fn skill_key(&self) -> &str {
        &self.spec.skill_key
    }

    /// Resolves `url_template` against `arguments`, then re-validates the result.
    ///
    /// The order is the point: substitute first, validate the **substituted** URL second.
    /// Validating the template would prove nothing about the URL that is actually
    /// requested, which is precisely risk R20.
    ///
    /// Argument placement follows one rule, chosen to match what
    /// `orchestration::openapi_import::build_params_schema` actually produces: a name the
    /// template names becomes a path segment, `body` becomes the JSON request body on the
    /// methods that carry one, and everything else becomes a query parameter. An OpenAPI
    /// parameter declared `in: header` therefore arrives as a query parameter — the
    /// importer flattens path/query/header into one property list and does not keep the
    /// `in` location, so nothing downstream can recover it. That is a documented
    /// simplification of the importer, not a decision taken here; the fix belongs in
    /// `params_schema`, which would let this function place headers correctly without
    /// changing its rule.
    async fn resolve_url(&self, arguments: &Map<String, Value>) -> Result<Url, SkillToolError> {
        let (raw_url, consumed) = substitute_path_placeholders(&self.spec.url_template, arguments)?;
        let mut url = Url::parse(&raw_url).map_err(|_| SkillToolError::UrlTemplate)?;

        // Everything the template did not consume, and that is not the request body,
        // becomes a query parameter. Deterministic order: the arguments map is iterated
        // as-is and appended in that order, so the same call always produces the same URL.
        //
        // Collected before touching `query_pairs_mut` because that borrow writes a `?` even
        // when nothing is appended, turning a clean `/orders/A-1` into `/orders/A-1?`.
        let query: Vec<(&str, String)> = arguments
            .iter()
            .filter(|(name, _)| !consumed.contains(name.as_str()) && *name != "body")
            .map(|(name, value)| (name.as_str(), scalar_to_query_value(value)))
            .collect();
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (name, value) in query {
                pairs.append_pair(name, &value);
            }
        }

        let policy = OutboundUrlPolicy {
            subject: "skill http executor url",
            dns_timeout: self.spec.outbound_policy.dns_timeout,
            // The stored `allowed_host` is the allow-list. It was derived server-side from
            // an already-validated URL at import or PATCH time and is not client-settable,
            // so pinning to it here is what stops an argument from redirecting the call.
            allowed_hosts: vec![self.spec.allowed_host.to_ascii_lowercase()],
            reject_credentials: true,
            allow_insecure: self.spec.outbound_policy.allow_insecure,
        };
        let validated = validate_outbound_url(url.as_str(), &policy, &SystemResolver)
            .await
            .map_err(|denial| {
                // `detail` can name a resolved internal address: server-side only, exactly
                // as `application::agent_platform::ssrf_blocked_error` does. The model gets
                // the class alone.
                tracing::warn!(
                    skill_key = %self.spec.skill_key,
                    reason = denial.reason().as_str(),
                    detail = denial.detail(),
                    "skill call blocked by the outbound SSRF policy"
                );
                SkillToolError::AddressNotPermitted
            })?;

        // `allow_insecure` short-circuits the guard's own allow-list check, so the host
        // equality is asserted here as well — the dev escape hatch must relax the address
        // *space*, never the "this skill may only talk to its own host" rule.
        let host = validated
            .host_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if host != self.spec.allowed_host.to_ascii_lowercase() {
            tracing::warn!(
                skill_key = %self.spec.skill_key,
                "skill call resolved to a host other than the executor's allowed_host"
            );
            return Err(SkillToolError::AddressNotPermitted);
        }
        Ok(validated)
    }
}

impl Tool for HttpSkillTool {
    // Every instance overrides `name()`, which is what `ToolSet` keys and advertises on.
    // `NAME` is only the fallback for a tool used outside a set.
    const NAME: &'static str = "moira_http_skill";

    type Error = SkillToolError;
    type Args = SkillToolArgs;
    type Output = SkillCallOutput;

    fn name(&self) -> String {
        self.spec.skill_key.clone()
    }

    fn description(&self) -> String {
        self.spec.description.clone()
    }

    fn parameters(&self) -> Value {
        self.spec.params_schema.clone()
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Unreachable on every dispatch path a `ToolSet` drives (`call_structured` ->
        // `call_with_extensions`), and harmless if reached directly: the caller scope only
        // adds request-correlation headers, so a scope-less call is a call without them.
        self.call_with_extensions(args, &ToolCallExtensions::new())
            .await
    }

    async fn call_with_extensions(
        &self,
        args: Self::Args,
        extensions: &ToolCallExtensions,
    ) -> Result<Self::Output, Self::Error> {
        let arguments = validate_arguments(&self.spec.params_schema, args.0)?;
        let url = self.resolve_url(&arguments).await?;

        let mut request = self
            .client
            .request(reqwest_method(self.spec.method), url)
            .header(reqwest::header::ACCEPT, "application/json");
        for (name, value) in
            header_pairs(&self.spec.header_template).map_err(|_| SkillToolError::UrlTemplate)?
        {
            request = request.header(name, value);
        }
        if let Some(credential) = &self.spec.credential {
            let (name, value) =
                authorization_header(credential).map_err(|_| SkillToolError::UrlTemplate)?;
            request = request.header(name, value.expose_secret());
        }
        if let Some(scope) = extensions.get::<SkillCallerScope>() {
            request = request.header("x-request-id", scope.request_id.clone());
            if let Some(tenant) = &scope.external_tenant_id {
                request = request.header("x-moira-tenant", tenant.clone());
            }
        }
        if method_carries_body(self.spec.method)
            && let Some(body) = arguments.get("body")
        {
            request = request.json(body);
        }

        let response = tokio::time::timeout(self.spec.timeout, request.send())
            .await
            .map_err(|_| SkillToolError::Timeout)?
            .map_err(|error| {
                if error.is_timeout() {
                    SkillToolError::Timeout
                } else {
                    SkillToolError::Transport
                }
            })?;

        let status = response.status();
        let bytes = tokio::time::timeout(self.spec.timeout, response.bytes())
            .await
            .map_err(|_| SkillToolError::Timeout)?
            .map_err(|_| SkillToolError::ResponseUnreadable)?;
        let truncated = bytes.len() > self.spec.maximum_response_bytes;
        let bounded = &bytes[..bytes.len().min(self.spec.maximum_response_bytes)];
        let text = String::from_utf8_lossy(bounded).into_owned();

        if !status.is_success() {
            // The body is deliberately dropped rather than forwarded: an upstream error
            // body is the classic place a target echoes back the `Authorization` header it
            // was sent. Class and status only, same posture as the Rig boundary.
            return Err(SkillToolError::Upstream {
                status: status.as_u16(),
            });
        }

        let body = if truncated {
            Value::String(text)
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        Ok(SkillCallOutput {
            status: status.as_u16(),
            body,
            truncated,
        })
    }

    fn classify_error(&self, error: &Self::Error) -> ToolFailure {
        match error {
            // The model can fix these by calling again with corrected arguments, so they
            // stay in-band as `InvalidArgs` rather than terminating the attempt.
            SkillToolError::InvalidArguments | SkillToolError::UrlTemplate => {
                ToolFailure::invalid_args(error.to_string())
            }
            SkillToolError::AddressNotPermitted => {
                ToolFailure::permission_denied(error.to_string())
                    .with_code("skill_address_not_permitted")
            }
            SkillToolError::Timeout => ToolFailure::timeout(error.to_string()),
            SkillToolError::Transport => ToolFailure::network(error.to_string()),
            SkillToolError::Upstream { status } => ToolFailure::provider(error.to_string())
                .with_http_status(*status)
                .with_retryable(*status >= 500),
            SkillToolError::ResponseUnreadable => ToolFailure::provider(error.to_string()),
        }
    }
}

fn reqwest_method(method: HttpMethod) -> reqwest::Method {
    match method {
        HttpMethod::Get => reqwest::Method::GET,
        HttpMethod::Post => reqwest::Method::POST,
        HttpMethod::Put => reqwest::Method::PUT,
        HttpMethod::Patch => reqwest::Method::PATCH,
        HttpMethod::Delete => reqwest::Method::DELETE,
    }
}

const fn method_carries_body(method: HttpMethod) -> bool {
    matches!(
        method,
        HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch
    )
}

/// `header_template` is documented as static, non-secret headers only
/// (`migrations/0031_agent_platform.sql`), so anything that is not a flat object of strings
/// is a configuration error rather than something to coerce.
fn header_pairs(template: &Value) -> Result<Vec<(String, String)>, SkillToolBuildError> {
    if template.is_null() {
        return Ok(Vec::new());
    }
    let object = template
        .as_object()
        .ok_or(SkillToolBuildError::InvalidHeaderTemplate)?;
    object
        .iter()
        .map(|(name, value)| {
            let value = value
                .as_str()
                .ok_or(SkillToolBuildError::InvalidHeaderTemplate)?;
            if name.eq_ignore_ascii_case("authorization") {
                // The credential is the only thing allowed to set this header; a template
                // that also sets it would either be a plaintext secret in a non-secret
                // column or a silent override of the resolved credential.
                return Err(SkillToolBuildError::InvalidHeaderTemplate);
            }
            Ok((name.clone(), value.to_string()))
        })
        .collect()
}

/// How a resolved credential becomes an HTTP header. Fail-closed: a credential type with no
/// unambiguous HTTP form makes the skill unusable rather than calling the target without it.
fn authorization_header(
    credential: &SkillCredential,
) -> Result<(&'static str, SecretString), SkillToolBuildError> {
    match credential.credential_type {
        CredentialType::ApiKey | CredentialType::BearerToken | CredentialType::Oauth2 => Ok((
            "authorization",
            SecretString::new(format!("Bearer {}", credential.secret.expose_secret())),
        )),
        _ => Err(SkillToolBuildError::UnsupportedCredentialType),
    }
}

/// `parameters()` is called on every request that advertises the tool, so this runs once at
/// construction instead. Providers reject a non-object parameter schema, and a `required`
/// name with no matching property is the schema bug that produces a model calling a tool
/// with an argument the target never accepts.
fn validate_parameters_schema(schema: &Value) -> Result<(), SkillToolBuildError> {
    let object = schema
        .as_object()
        .ok_or(SkillToolBuildError::InvalidParametersSchema)?;
    if object.get("type").and_then(Value::as_str) != Some("object") {
        return Err(SkillToolBuildError::InvalidParametersSchema);
    }
    let properties = match object.get("properties") {
        Some(Value::Object(properties)) => properties,
        _ => return Err(SkillToolBuildError::InvalidParametersSchema),
    };
    let required = match object.get("required") {
        None | Some(Value::Null) => return Ok(()),
        Some(Value::Array(required)) => required,
        Some(_) => return Err(SkillToolBuildError::InvalidParametersSchema),
    };
    for name in required {
        let name = name
            .as_str()
            .ok_or(SkillToolBuildError::InvalidParametersSchema)?;
        if !properties.contains_key(name) {
            return Err(SkillToolBuildError::InvalidParametersSchema);
        }
    }
    Ok(())
}

/// Validates the model's arguments against `params_schema`.
///
/// Structural, not a full JSON-Schema validator: every declared `required` property must be
/// present, and **no** property outside `properties` is accepted. The second half is the
/// security-relevant one — an unknown argument that survived to `resolve_url` would become
/// a query parameter the operator never declared.
fn validate_arguments(
    schema: &Value,
    arguments: Value,
) -> Result<Map<String, Value>, SkillToolError> {
    let arguments = match arguments {
        // Rig already normalises a bare `null` to `{}` before deserializing `Args`; this
        // covers the same shape arriving as an explicit JSON null inside the object.
        Value::Null => Map::new(),
        Value::Object(map) => map,
        _ => return Err(SkillToolError::InvalidArguments),
    };
    let object = schema.as_object().ok_or(SkillToolError::InvalidArguments)?;
    let properties = object
        .get("properties")
        .and_then(Value::as_object)
        .ok_or(SkillToolError::InvalidArguments)?;
    for name in arguments.keys() {
        if !properties.contains_key(name) {
            return Err(SkillToolError::InvalidArguments);
        }
    }
    if let Some(Value::Array(required)) = object.get("required") {
        for name in required {
            let name = name.as_str().ok_or(SkillToolError::InvalidArguments)?;
            if !arguments.contains_key(name) {
                return Err(SkillToolError::InvalidArguments);
            }
        }
    }
    Ok(arguments)
}

/// Replaces every `{name}` in `template` with the percent-encoded argument of that name,
/// returning the resolved URL and the argument names the path consumed.
///
/// Percent-encoding with `NON_ALPHANUMERIC` on purpose: `/`, `?`, `#` and `:` are all
/// encoded, so a value like `../../admin` or `x?a=b` cannot climb out of its path segment
/// or graft a query string onto the URL. A placeholder with no argument is an error, never
/// an empty segment.
fn substitute_path_placeholders<'a>(
    template: &str,
    arguments: &'a Map<String, Value>,
) -> Result<(String, HashSet<&'a str>), SkillToolError> {
    let mut resolved = String::with_capacity(template.len());
    let mut consumed = HashSet::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let end = rest[start..]
            .find('}')
            .map(|offset| start + offset)
            .ok_or(SkillToolError::UrlTemplate)?;
        resolved.push_str(&rest[..start]);
        let name = &rest[start + 1..end];
        let (key, value) = arguments
            .get_key_value(name)
            .ok_or(SkillToolError::UrlTemplate)?;
        let rendered = scalar_to_query_value(value);
        if rendered.is_empty() {
            return Err(SkillToolError::UrlTemplate);
        }
        resolved.push_str(&percent_encode_path_segment(&rendered));
        consumed.insert(key.as_str());
        rest = &rest[end + 1..];
    }
    resolved.push_str(rest);
    Ok((resolved, consumed))
}

/// Percent-encodes one path segment, keeping only the characters that can never change a
/// URL's structure: ASCII alphanumerics, `-`, `_` and `~`.
///
/// Hand-rolled rather than pulling in `percent-encoding` as a direct dependency (it is only
/// a transitive one today, and `deny.toml`/`tests/supply_chain_policy.rs` govern the direct
/// set). **`.` is encoded on purpose**, which is the whole point: `url::Url::parse`
/// normalises literal `.` and `..` segments away, so an unencoded `../..` in a
/// model-supplied argument would rewrite the path before the request is ever made, while
/// `%2E%2E` is carried through verbatim as an ordinary segment.
fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'~' => {
                encoded.push(*byte as char);
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// How a JSON argument renders into a URL. Strings pass through; scalars render as
/// themselves; an array or object has no unambiguous URL form and renders as compact JSON,
/// which is at least round-trippable rather than `[object Object]`-shaped.
fn scalar_to_query_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Builds the `ToolSet` and the `Vec<ToolDefinition>` one request advertises.
///
/// Uniqueness is validated **before** construction rather than trusting `ToolSet`'s own
/// behaviour: re-registering a name replaces it in place and only logs a `warn!`, so a
/// duplicate `skill_key` would silently mean one of two configured skills is unreachable.
pub fn build_skill_tool_set(
    specs: Vec<SkillToolSpec>,
    client: reqwest::Client,
) -> Result<(ToolSet, Vec<ToolDefinition>), (String, SkillToolBuildError)> {
    let mut seen = HashSet::new();
    let mut builder = ToolSet::builder();
    for spec in specs {
        let skill_key = spec.skill_key.clone();
        if !seen.insert(skill_key.clone()) {
            return Err((skill_key, SkillToolBuildError::DuplicateSkillKey));
        }
        let tool =
            HttpSkillTool::new(spec, client.clone()).map_err(|error| (skill_key.clone(), error))?;
        builder = builder.static_tool(tool);
    }
    let tools = builder.build();
    let definitions = tools
        .get_tool_definitions()
        // `get_tool_definitions` is infallible for the static tools built above (it only
        // reads `name`/`description`/`parameters`), so this arm is unreachable in practice
        // and is mapped rather than unwrapped so it can never panic in production.
        .map_err(|_| (String::new(), SkillToolBuildError::InvalidParametersSchema))?;
    Ok((tools, definitions))
}

/// What one turn of the loop did, for the runtime event stream and the attempt record.
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub tool_name: String,
    /// Rig's own vocabulary verbatim: `success` | `error` | `skipped` | `denied`. Not a
    /// parallel Moira vocabulary, so the two telemetries stay comparable.
    pub outcome: &'static str,
    pub failure_kind: Option<&'static str>,
    /// Set when a Moira guard, rather than the tool, refused the call.
    pub guard_key: Option<String>,
    pub guard_reason: Option<&'static str>,
}

/// The answer a tool-bearing execution produced.
#[derive(Debug)]
pub struct ToolLoopOutcome {
    pub text: String,
    pub usage: crate::domain::UsageSummary,
    pub provider_request_id: Option<String>,
    /// Model calls made, including the final one that produced `text`.
    pub turns: usize,
    pub tool_calls: Vec<ToolCallRecord>,
}

/// Everything the loop needs besides the model handle and the request it re-issues.
///
/// One borrowed struct rather than six parameters: the six travel together through
/// `application::execution` and back into this module, and a positional list of that length
/// is the shape where two same-typed slices (`guards`, `caller_scopes`) get silently
/// swapped at a call site.
pub struct ToolLoopContext<'a> {
    pub tools: &'a ToolSet,
    /// What the request advertises, in `skill_refs` order.
    pub definitions: &'a [ToolDefinition],
    /// Evaluated before every dispatch, in order; the first denial wins.
    pub guards: &'a [SkillGuard],
    /// The caller's already-granted scopes. Read-only input to the guards — nothing here
    /// can add a scope, which is what keeps guards narrowing-only.
    pub caller_scopes: &'a [String],
    /// Moira-authored per-call context, never serialized to the model.
    pub extensions: &'a ToolCallExtensions,
    /// Counts **model calls**, matching Rig's own semantics: one tool call plus a final
    /// answer needs at least 2.
    pub maximum_tool_turns: usize,
}

/// Drives the multi-turn tool loop against `handle`.
///
/// Moira drives this rather than `rig_core::agent::AgentRunner` because Moira owns retries,
/// deadlines, circuit breaking, permits and cancellation — see
/// `.agents/skills/moira-rig-agents-rag/SKILL.md`, whose default answer for the public path
/// is to stay at the `CompletionModel` level.
pub async fn run_tool_loop(
    handle: &RuntimeModelHandle,
    mut request: CompletionRequest,
    context: ToolLoopContext<'_>,
) -> Result<ToolLoopOutcome, ExecutionFailure> {
    let ToolLoopContext {
        tools,
        definitions,
        guards,
        caller_scopes,
        extensions,
        maximum_tool_turns,
    } = context;
    if maximum_tool_turns == 0 {
        return Err(ExecutionFailure::new(
            ExecutionFailureClass::InvalidExecutionRequest,
            "tool turn budget must be at least one",
        ));
    }
    let mut history: Vec<Message> = request.chat_history.iter().cloned().collect();
    let mut tool_calls = Vec::new();

    for turn in 1..=maximum_tool_turns {
        request.tools = definitions.to_vec();
        request.chat_history = OneOrMany::many(history.clone()).map_err(|_| {
            ExecutionFailure::new(
                ExecutionFailureClass::InvalidExecutionRequest,
                "execution command must contain at least one message",
            )
        })?;

        let output = handle.completion(request.clone()).await?;
        if output.tool_calls.is_empty() {
            return Ok(ToolLoopOutcome {
                text: output.text,
                usage: output.usage,
                provider_request_id: output.provider_request_id,
                turns: turn,
                tool_calls,
            });
        }

        // One assistant message carrying every tool call of the turn ...
        let assistant_content: Vec<AssistantContent> = output
            .tool_calls
            .iter()
            .cloned()
            .map(AssistantContent::ToolCall)
            .collect();
        let content = OneOrMany::many(assistant_content).map_err(|_| {
            ExecutionFailure::new(
                ExecutionFailureClass::ProviderInvalidResponse,
                "provider returned an empty assistant turn",
            )
        })?;
        history.push(Message::Assistant {
            id: output.provider_request_id.clone(),
            content,
        });

        // ... then exactly one user message carrying every tool result, in call order.
        // Providers require that shape for parallel tool calls; Anthropic rejects
        // tool_result blocks split across several user turns.
        let mut results: Vec<UserContent> = Vec::with_capacity(output.tool_calls.len());
        for tool_call in &output.tool_calls {
            let (text, record) =
                execute_one_tool_call(tools, guards, caller_scopes, tool_call, extensions).await;
            tool_calls.push(record);
            let content = OneOrMany::one(ToolResultContent::text(text));
            results.push(match tool_call.call_id.clone() {
                Some(call_id) => {
                    UserContent::tool_result_with_call_id(tool_call.id.clone(), call_id, content)
                }
                None => UserContent::tool_result(tool_call.id.clone(), content),
            });
        }
        let content = OneOrMany::many(results).map_err(|_| {
            ExecutionFailure::new(
                ExecutionFailureClass::InternalError,
                "tool execution produced no tool results",
            )
        })?;
        history.push(Message::User { content });
    }

    Err(ExecutionFailure::new(
        ExecutionFailureClass::DeadlineExceeded,
        "execution exceeded the configured tool turn budget",
    ))
}

/// Guards first, then dispatch. Both halves stay in-band: a denial is a tool result the
/// model can read and recover from, which costs one turn instead of failing the execution
/// a caller may have already received output for.
async fn execute_one_tool_call(
    tools: &ToolSet,
    guards: &[SkillGuard],
    caller_scopes: &[String],
    tool_call: &ToolCall,
    extensions: &ToolCallExtensions,
) -> (String, ToolCallRecord) {
    let tool_name = tool_call.function.name.clone();
    if let GuardVerdict::Deny { guard_key, reason } =
        evaluate_guards(guards, &tool_name, GuardContext { caller_scopes })
    {
        tracing::info!(
            tool_name = %tool_name,
            guard_key = %guard_key,
            reason = reason.as_str(),
            "skill call denied by a guard"
        );
        let denial = json!({
            "status": "denied",
            "error": {
                "code": "skill_guard_denied",
                "message_key": "moira.error.skill_guard_denied",
                "reason": reason.as_str(),
            }
        })
        .to_string();
        return (
            denial,
            ToolCallRecord {
                tool_name,
                outcome: "denied",
                failure_kind: None,
                guard_key: Some(guard_key),
                guard_reason: Some(reason.as_str()),
            },
        );
    }

    let execution = tools
        .call_structured(
            &tool_name,
            tool_call.function.arguments.to_string(),
            extensions,
        )
        .await;
    let outcome = execution.outcome();
    let failure_kind = outcome.error_kind().map(|kind| kind.as_str());
    // Arguments and output are deliberately absent from this line: Rig itself logs the
    // arguments at `debug` under target `rig`, and repeating them at `info` would put a
    // model-authored payload into production logs.
    tracing::info!(
        tool_name = %tool_name,
        outcome = outcome.as_str(),
        failure_kind,
        "skill call completed"
    );
    let record = ToolCallRecord {
        tool_name,
        outcome: outcome.as_str(),
        failure_kind,
        guard_key: None,
        guard_reason: None,
    };
    (execution.model_output().to_string(), record)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(url_template: &str, params_schema: Value) -> SkillToolSpec {
        SkillToolSpec {
            skill_key: "orders_get".to_string(),
            description: "look an order up".to_string(),
            params_schema,
            method: HttpMethod::Get,
            url_template: url_template.to_string(),
            allowed_host: "api.example.test".to_string(),
            header_template: json!({}),
            credential: None,
            timeout: Duration::from_millis(500),
            maximum_response_bytes: 1024,
            outbound_policy: SkillOutboundPolicy {
                dns_timeout: Duration::from_millis(200),
                allow_insecure: false,
            },
        }
    }

    fn object_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "order_id": {"type": "string"},
                "page": {"type": "integer"}
            },
            "required": ["order_id"],
            "additionalProperties": false
        })
    }

    #[test]
    fn a_tool_advertises_its_skill_key_and_its_stored_schema() {
        let tool = HttpSkillTool::new(
            spec(
                "https://api.example.test/orders/{order_id}",
                object_schema(),
            ),
            reqwest::Client::new(),
        )
        .expect("a well-formed skill builds");
        assert_eq!(Tool::name(&tool), "orders_get");
        assert_eq!(tool.description(), "look an order up");
        assert_eq!(tool.parameters(), object_schema());
    }

    /// The schema guard runs at construction, not per request: `parameters()` is called on
    /// every advertised request and must stay allocation-cheap and side-effect free.
    #[test]
    fn a_parameter_schema_that_no_provider_would_accept_fails_at_construction() {
        for schema in [
            json!("string"),
            json!({"type": "array", "properties": {}}),
            json!({"type": "object"}),
            json!({
                "type": "object",
                "properties": {"a": {"type": "string"}},
                "required": ["b"]
            }),
        ] {
            assert_eq!(
                HttpSkillTool::new(
                    spec("https://api.example.test/orders", schema.clone()),
                    reqwest::Client::new()
                )
                .map(|_| ())
                .expect_err("an invalid parameter schema must be refused"),
                SkillToolBuildError::InvalidParametersSchema,
                "schema {schema} was accepted"
            );
        }
    }

    /// `header_template` is a non-secret column. A template that sets `Authorization`
    /// either stores a plaintext secret there or silently overrides the resolved
    /// credential; both are configuration faults, so the skill does not build.
    #[test]
    fn a_header_template_cannot_set_authorization_or_carry_a_non_string() {
        let mut authorization = spec("https://api.example.test/orders", object_schema());
        authorization.header_template = json!({"Authorization": "Bearer leaked"});
        assert_eq!(
            HttpSkillTool::new(authorization, reqwest::Client::new())
                .map(|_| ())
                .expect_err("an Authorization header template must be refused"),
            SkillToolBuildError::InvalidHeaderTemplate
        );

        let mut non_string = spec("https://api.example.test/orders", object_schema());
        non_string.header_template = json!({"X-Trace": 7});
        assert_eq!(
            HttpSkillTool::new(non_string, reqwest::Client::new())
                .map(|_| ())
                .expect_err("a non-string header value must be refused"),
            SkillToolBuildError::InvalidHeaderTemplate
        );
    }

    /// Fail-closed on credentials: a type with no unambiguous HTTP form makes the skill
    /// unusable rather than calling the target unauthenticated.
    #[test]
    fn a_credential_type_with_no_http_form_refuses_to_build_a_tool() {
        let mut unusable = spec("https://api.example.test/orders", object_schema());
        unusable.credential = Some(SkillCredential {
            credential_type: CredentialType::ServiceAccount,
            secret: SecretString::new("sk-skill-secret".to_string()),
        });
        assert_eq!(
            HttpSkillTool::new(unusable, reqwest::Client::new())
                .map(|_| ())
                .expect_err("an unsupported credential type must be refused"),
            SkillToolBuildError::UnsupportedCredentialType
        );
    }

    /// No `Debug` rendering anywhere in this module may contain the secret. Asserted
    /// against the literal, the way `tests/execution_lifecycle.rs` asserts against
    /// `sk-lifecycle-secret`.
    #[test]
    fn no_debug_rendering_of_a_skill_can_expose_its_secret() {
        const SECRET: &str = "sk-skill-debug-secret";
        let mut with_credential = spec("https://api.example.test/orders", object_schema());
        with_credential.credential = Some(SkillCredential {
            credential_type: CredentialType::ApiKey,
            secret: SecretString::new(SECRET.to_string()),
        });
        let rendered = format!("{with_credential:?}");
        assert!(!rendered.contains(SECRET), "spec Debug leaked the secret");

        let tool = HttpSkillTool::new(with_credential, reqwest::Client::new()).expect("tool");
        assert!(
            !format!("{tool:?}").contains(SECRET),
            "tool Debug leaked the secret"
        );
        let credential = SkillCredential {
            credential_type: CredentialType::ApiKey,
            secret: SecretString::new(SECRET.to_string()),
        };
        assert!(
            !format!("{credential:?}").contains(SECRET),
            "credential Debug leaked the secret"
        );
    }

    #[test]
    fn unknown_and_missing_arguments_are_both_refused() {
        let schema = object_schema();
        assert!(matches!(
            validate_arguments(&schema, json!({"page": 2})),
            Err(SkillToolError::InvalidArguments)
        ));
        assert!(matches!(
            validate_arguments(&schema, json!({"order_id": "a", "sneaky": "x"})),
            Err(SkillToolError::InvalidArguments)
        ));
        assert!(matches!(
            validate_arguments(&schema, json!(["order_id"])),
            Err(SkillToolError::InvalidArguments)
        ));
        let accepted = validate_arguments(&schema, json!({"order_id": "a-1", "page": 2}))
            .expect("declared arguments are accepted");
        assert_eq!(accepted.len(), 2);
    }

    /// The path-traversal half. `NON_ALPHANUMERIC` encoding means an argument cannot climb
    /// out of its segment, graft a query string on, or change the host — the three ways a
    /// model-authored string could otherwise redirect the call.
    #[test]
    fn a_path_argument_cannot_escape_its_segment() {
        let mut arguments = Map::new();
        arguments.insert("order_id".to_string(), json!("../../admin?x=1"));
        let (resolved, consumed) = substitute_path_placeholders(
            "https://api.example.test/v1/orders/{order_id}",
            &arguments,
        )
        .expect("substitution succeeds");
        assert_eq!(
            resolved,
            "https://api.example.test/v1/orders/%2E%2E%2F%2E%2E%2Fadmin%3Fx%3D1"
        );
        assert!(consumed.contains("order_id"));
        let url = Url::parse(&resolved).expect("a valid url");
        assert_eq!(url.host_str(), Some("api.example.test"));
        assert_eq!(url.query(), None);
    }

    #[test]
    fn a_placeholder_with_no_argument_is_an_error_rather_than_an_empty_segment() {
        let arguments = Map::new();
        assert!(matches!(
            substitute_path_placeholders("https://api.example.test/o/{order_id}", &arguments),
            Err(SkillToolError::UrlTemplate)
        ));

        let mut empty = Map::new();
        empty.insert("order_id".to_string(), json!(""));
        assert!(matches!(
            substitute_path_placeholders("https://api.example.test/o/{order_id}", &empty),
            Err(SkillToolError::UrlTemplate)
        ));
    }

    /// Two skills advertising one name would silently replace each other inside `ToolSet`
    /// with only a `warn!`. Refused instead.
    #[test]
    fn two_skills_with_the_same_key_refuse_to_build_a_tool_set() {
        let specs = vec![
            spec("https://api.example.test/a", object_schema()),
            spec("https://api.example.test/b", object_schema()),
        ];
        let (key, error) = build_skill_tool_set(specs, reqwest::Client::new())
            .map(|_| ())
            .expect_err("a duplicate skill_key must be refused");
        assert_eq!(key, "orders_get");
        assert_eq!(error, SkillToolBuildError::DuplicateSkillKey);
    }

    #[test]
    fn tool_definitions_follow_registration_order() {
        let mut second = spec("https://api.example.test/b", object_schema());
        second.skill_key = "orders_list".to_string();
        let (_, definitions) = build_skill_tool_set(
            vec![spec("https://api.example.test/a", object_schema()), second],
            reqwest::Client::new(),
        )
        .expect("a well-formed tool set builds");
        assert_eq!(
            definitions
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>(),
            vec!["orders_get", "orders_list"]
        );
    }

    /// Execution-time SSRF (risk R20), proved on the *resolved* URL: the template names a
    /// permitted host, and the loopback address only appears after substitution. An
    /// implementation that validated the template would let this through.
    #[tokio::test]
    async fn a_skill_call_to_a_denied_address_is_refused_at_execution_time() {
        let mut loopback = spec(
            "https://api.example.test/proxy/{target}",
            json!({
                "type": "object",
                "properties": {"target": {"type": "string"}},
                "required": ["target"]
            }),
        );
        loopback.url_template = "https://127.0.0.1/orders/{order_id}".to_string();
        loopback.allowed_host = "127.0.0.1".to_string();
        loopback.params_schema = json!({
            "type": "object",
            "properties": {"order_id": {"type": "string"}},
            "required": ["order_id"]
        });
        let tool = HttpSkillTool::new(loopback, reqwest::Client::new()).expect("tool builds");
        let error = tool
            .call_with_extensions(
                SkillToolArgs(json!({"order_id": "a-1"})),
                &ToolCallExtensions::new(),
            )
            .await
            .expect_err("a loopback target must be refused");
        assert!(matches!(error, SkillToolError::AddressNotPermitted));
        assert_eq!(
            tool.classify_error(&error).kind,
            rig_core::tool::ToolFailureKind::PermissionDenied
        );
    }

    /// The scheme half of the same guard: `http://` is refused even for a public host.
    #[tokio::test]
    async fn a_plaintext_http_skill_target_is_refused() {
        let mut insecure = spec(
            "http://api.example.test/orders",
            json!({"type": "object", "properties": {}}),
        );
        insecure.url_template = "http://8.8.8.8/orders".to_string();
        insecure.allowed_host = "8.8.8.8".to_string();
        let tool = HttpSkillTool::new(insecure, reqwest::Client::new()).expect("tool builds");
        let error = tool
            .call_with_extensions(SkillToolArgs(json!({})), &ToolCallExtensions::new())
            .await
            .expect_err("an http target must be refused");
        assert!(matches!(error, SkillToolError::AddressNotPermitted));
    }

    #[test]
    fn every_error_variant_maps_onto_the_failure_kind_its_remedy_implies() {
        use rig_core::tool::ToolFailureKind;
        let tool = HttpSkillTool::new(
            spec("https://api.example.test/orders", object_schema()),
            reqwest::Client::new(),
        )
        .expect("tool builds");
        let cases = [
            (
                SkillToolError::InvalidArguments,
                ToolFailureKind::InvalidArgs,
            ),
            (SkillToolError::UrlTemplate, ToolFailureKind::InvalidArgs),
            (
                SkillToolError::AddressNotPermitted,
                ToolFailureKind::PermissionDenied,
            ),
            (SkillToolError::Timeout, ToolFailureKind::Timeout),
            (SkillToolError::Transport, ToolFailureKind::Network),
            (
                SkillToolError::Upstream { status: 500 },
                ToolFailureKind::Provider,
            ),
            (
                SkillToolError::ResponseUnreadable,
                ToolFailureKind::Provider,
            ),
        ];
        for (error, expected) in cases {
            let failure = tool.classify_error(&error);
            assert_eq!(failure.kind, expected, "{error} classified wrongly");
            assert!(
                !failure.message.is_empty(),
                "{error} produced an empty classification message"
            );
        }
        assert_eq!(
            tool.classify_error(&SkillToolError::Upstream { status: 500 })
                .retryable,
            Some(true)
        );
        assert_eq!(
            tool.classify_error(&SkillToolError::Upstream { status: 404 })
                .retryable,
            Some(false)
        );
    }
}
