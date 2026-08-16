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
//! attempt impossible (an exhausted turn budget, cancellation) become an `ExecutionFailure`
//! — and that one carries what the loop already spent and dispatched, see [`ToolLoopFailure`].
//!
//! # A retried attempt replays the tool calls
//!
//! `ProviderTimeout` and `ProviderConnectionFailed` are retryable and fallback-eligible
//! (`orchestration::controls`), so a provider failure on turn 3 fails the attempt and
//! `application::execution` starts a fresh one — with an empty history, so the model
//! re-issues the calls the previous attempt already dispatched. The only correlator sent is
//! `x-request-id`, which is request correlation and not an idempotency key, and nothing
//! obliges a target to honour it. **A skill executor whose method carries a body must
//! therefore be idempotent.** Moira cannot enforce that on a third-party endpoint; what it
//! can do, and now does, is keep the record of every dispatch that did happen even when the
//! attempt around it failed.

use std::{collections::HashSet, time::Duration};

use futures_util::StreamExt;
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
        HttpMethod, SkillGuard, UsageSummary, evaluate_guards,
    },
    orchestration::{RuntimeModelHandle, openapi_import::body_property_name},
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
    /// Which argument carries the JSON request body, read out of `params_schema` once at
    /// construction by `openapi_import::body_property_name` — the importer's own answer,
    /// asked rather than assumed. Nothing in this module spells the name out; hard-coding
    /// `"body"` here while the importer renamed a colliding body is precisely what inverted
    /// the dispatch once already.
    body_property: Option<String>,
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
        let body_property = body_property_name(&spec.params_schema).map(str::to_string);
        Ok(Self {
            spec,
            client,
            body_property,
        })
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
    /// template names becomes a path segment, the property
    /// `orchestration::openapi_import::body_property_name` identifies becomes the JSON
    /// request body on the methods that carry one, and everything else becomes a query
    /// parameter. That property is usually — but not always — literally `body`. An OpenAPI
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
        let body_property = self.body_property.as_deref();
        let query: Vec<(&str, String)> = arguments
            .iter()
            .filter(|(name, _)| {
                !consumed.contains(name.as_str()) && Some(name.as_str()) != body_property
            })
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
            && let Some(body) = self
                .body_property
                .as_deref()
                .and_then(|name| arguments.get(name))
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
        if !status.is_success() {
            // The body is deliberately dropped rather than forwarded: an upstream error
            // body is the classic place a target echoes back the `Authorization` header it
            // was sent. Class and status only, same posture as the Rig boundary.
            //
            // Decided *before* the body is read, not after: nothing here will ever look at
            // those bytes, so reading them would be pure cost. Dropping `response` unread
            // ends the transfer.
            return Err(SkillToolError::Upstream {
                status: status.as_u16(),
            });
        }

        let (text, truncated) = tokio::time::timeout(
            self.spec.timeout,
            read_bounded_body(response, self.spec.maximum_response_bytes),
        )
        .await
        .map_err(|_| SkillToolError::Timeout)?
        .map_err(|_| SkillToolError::ResponseUnreadable)?;

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

/// Reads at most `maximum_bytes` of a response body, and stops pulling once it has them.
///
/// **`maximum_response_bytes` is a memory bound, not a formatting rule.** The obvious
/// spelling — `response.bytes().await` and slice afterwards — buffers the *whole* body first,
/// so a target answering 200 with a 2 GB chunked stream puts 2 GB on the heap before the
/// ceiling is consulted, and `spec.timeout` does not help because it bounds duration, not
/// bytes. `security::ssrf::fetch_jwks_hardened` already carries that warning verbatim for a
/// URL an admin configured; a skill target is less trusted than that, and this is the same
/// counter.
///
/// Returning at the ceiling drops the stream, which ends the transfer mid-body. No
/// `Content-Length` pre-check: a declared over-cap length is not an error here, it is exactly
/// the truncation `SkillCallOutput::truncated` exists to report, and refusing it would turn a
/// body that used to be usable-but-partial into a failed tool call.
async fn read_bounded_body(
    response: reqwest::Response,
    maximum_bytes: usize,
) -> Result<(String, bool), reqwest::Error> {
    let mut stream = response.bytes_stream();
    let mut buffered: Vec<u8> = Vec::with_capacity(maximum_bytes.min(8 * 1024));
    let mut truncated = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let remaining = maximum_bytes.saturating_sub(buffered.len());
        if chunk.len() > remaining {
            // Kept byte-for-byte identical to what the buffer-then-slice version produced,
            // so only the memory ceiling moves: the first `maximum_bytes` bytes, flagged.
            buffered.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        buffered.extend_from_slice(&chunk);
    }
    drop(stream);
    Ok((String::from_utf8_lossy(&buffered).into_owned(), truncated))
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
    pub usage: UsageSummary,
    pub provider_request_id: Option<String>,
    /// Model calls made, including the final one that produced `text`.
    pub turns: usize,
    pub tool_calls: Vec<ToolCallRecord>,
}

/// A tool loop that could not finish, **plus everything it had already spent and done**.
///
/// A bare [`ExecutionFailure`] was wrong here for the same reason it was wrong on the
/// structured-output path (`application::execution::FailedAttempt`, issue #80): by the time
/// this loop fails it may have made several billed provider calls and dispatched real
/// outbound requests, and `HttpMethod` admits `Post`/`Put`/`Patch`/`Delete`. Dropping the
/// records left a mutation of an operator's third-party API with no trace on any surface —
/// no runtime event, no attempt metadata — and dropping the counts billed those provider
/// calls as `UsageSummary::default()`, which reads as *unknown* and skips the
/// `usage_records` row entirely.
#[derive(Debug)]
pub struct ToolLoopFailure {
    pub failure: ExecutionFailure,
    /// Every turn that answered, summed.
    ///
    /// A sum rather than "the last turn's", which is what [`ToolLoopOutcome`] reports:
    /// there is no answering turn on this path, so the only honest figure is the total of
    /// the calls that were actually made and invoiced. (Whether the success path should sum
    /// too is a separate, filed question — see issue #252 finding 4.)
    pub usage: UsageSummary,
    /// Tool calls dispatched before the failure, in call order.
    pub tool_calls: Vec<ToolCallRecord>,
}

impl ToolLoopFailure {
    fn new(
        class: ExecutionFailureClass,
        message: &'static str,
        usage: UsageSummary,
        tool_calls: Vec<ToolCallRecord>,
    ) -> Self {
        Self {
            failure: ExecutionFailure::new(class, message),
            usage,
            tool_calls,
        }
    }
}

/// Folds one turn's counts into the running total.
///
/// `Option` semantics are `UsageSummary`'s own and match
/// `application::execution::usage_was_reported`: `None` means the provider said nothing, not
/// that it charged nothing. So a turn that reported nothing must not erase a turn that did —
/// `None` is only preserved where every turn was `None`. Saturating because these are
/// provider-supplied numbers and a wrap would be a worse lie than a clamp.
fn accumulate_usage(total: &mut UsageSummary, turn: &UsageSummary) {
    fn add(total: &mut Option<u64>, turn: Option<u64>) {
        if let Some(value) = turn {
            *total = Some(total.unwrap_or(0).saturating_add(value));
        }
    }
    add(&mut total.input_tokens, turn.input_tokens);
    add(&mut total.output_tokens, turn.output_tokens);
    add(&mut total.cached_input_tokens, turn.cached_input_tokens);
    add(&mut total.reasoning_tokens, turn.reasoning_tokens);
    add(&mut total.total_tokens, turn.total_tokens);
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
///
/// **Every exit carries what the loop did.** The failure type is [`ToolLoopFailure`], not a
/// bare [`ExecutionFailure`], so the dispatched-call records and the accumulated token counts
/// survive an exit through any arm rather than only the answering one; `?` is deliberately
/// not used on the provider call for that reason.
pub async fn run_tool_loop(
    handle: &RuntimeModelHandle,
    mut request: CompletionRequest,
    context: ToolLoopContext<'_>,
) -> Result<ToolLoopOutcome, ToolLoopFailure> {
    let ToolLoopContext {
        tools,
        definitions,
        guards,
        caller_scopes,
        extensions,
        maximum_tool_turns,
    } = context;
    if maximum_tool_turns == 0 {
        return Err(ToolLoopFailure::new(
            ExecutionFailureClass::InvalidExecutionRequest,
            "tool turn budget must be at least one",
            UsageSummary::default(),
            Vec::new(),
        ));
    }
    let mut history: Vec<Message> = request.chat_history.iter().cloned().collect();
    let mut tool_calls = Vec::new();
    let mut spent = UsageSummary::default();

    for turn in 1..=maximum_tool_turns {
        request.tools = definitions.to_vec();
        let Ok(chat_history) = OneOrMany::many(history.clone()) else {
            return Err(ToolLoopFailure::new(
                ExecutionFailureClass::InvalidExecutionRequest,
                "execution command must contain at least one message",
                spent,
                tool_calls,
            ));
        };
        request.chat_history = chat_history;

        let output = match handle.completion(request.clone()).await {
            Ok(output) => output,
            // The turns before this one were answered and billed, and any tool calls they
            // made have already left the process. Both travel with the failure.
            Err(failure) => {
                return Err(ToolLoopFailure {
                    failure,
                    usage: spent,
                    tool_calls,
                });
            }
        };
        accumulate_usage(&mut spent, &output.usage);
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
        let Ok(content) = OneOrMany::many(assistant_content) else {
            return Err(ToolLoopFailure::new(
                ExecutionFailureClass::ProviderInvalidResponse,
                "provider returned an empty assistant turn",
                spent,
                tool_calls,
            ));
        };
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
        let Ok(content) = OneOrMany::many(results) else {
            return Err(ToolLoopFailure::new(
                ExecutionFailureClass::InternalError,
                "tool execution produced no tool results",
                spent,
                tool_calls,
            ));
        };
        history.push(Message::User { content });
    }

    // The budget-exhaustion exit is the one that discards the most: `maximum_tool_turns`
    // billed completions and every tool call they asked for, all of which really happened.
    Err(ToolLoopFailure::new(
        ExecutionFailureClass::DeadlineExceeded,
        "execution exceeded the configured tool turn budget",
        spent,
        tool_calls,
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

    /// **`maximum_response_bytes` bounds memory, not just what the model is shown.**
    ///
    /// What is asserted is deliberately *not* the truncation — the buffer-then-slice version
    /// truncated too, so a truncation test passes either way and proves nothing. It is how much
    /// of the body the target managed to hand out before Moira stopped pulling. Awaiting
    /// `response.bytes()` made that figure the whole body, every byte of it resident at once,
    /// with `spec.timeout` bounding only the duration; a target that answers 200 with a 2 GB
    /// chunked stream then decides how much heap Moira uses.
    ///
    /// 64 MiB behind a 4 KiB cap, and the pass bar is a quarter of the body — loose on purpose,
    /// because the kernel's socket buffer and hyper's own read-ahead legitimately overshoot the
    /// cap by megabytes, while the defect overshoots it by the entire body.
    #[tokio::test]
    async fn a_response_far_larger_than_the_cap_is_never_buffered_whole() {
        const CHUNK: usize = 64 * 1024;
        const CHUNKS: usize = 1_024;
        const CAP: usize = 4 * 1024;

        let served = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let address = start_oversize_target(OversizeTarget {
            served: served.clone(),
            chunk: CHUNK,
            chunks: CHUNKS,
        })
        .await;

        let mut oversized = spec(
            &format!("http://{address}/orders/{{order_id}}"),
            json!({
                "type": "object",
                "properties": {"order_id": {"type": "string"}},
                "required": ["order_id"]
            }),
        );
        oversized.allowed_host = "127.0.0.1".to_string();
        oversized.maximum_response_bytes = CAP;
        // A loopback `http://` target needs the same dev escape hatch
        // `tests/skill_tool_loop.rs` runs under, and for the same reason.
        oversized.outbound_policy.allow_insecure = true;
        // Generous on purpose: the read must end because of the ceiling, not the clock. A
        // 500 ms budget would let the old implementation fail as a `Timeout` and hide which
        // of the two bounds actually stopped it.
        oversized.timeout = Duration::from_secs(20);

        let tool = HttpSkillTool::new(oversized, reqwest::Client::new()).expect("tool builds");
        let output = tool
            .call_with_extensions(
                SkillToolArgs(json!({"order_id": "A-1"})),
                &ToolCallExtensions::new(),
            )
            .await
            .expect("an over-cap body is truncated, not an error");

        assert!(output.truncated, "an over-cap body must be flagged partial");
        assert_eq!(
            output.body.as_str().map(str::len),
            Some(CAP),
            "the model must be shown exactly the ceiling"
        );
        let served = served.load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            served < CHUNK * CHUNKS / 4,
            "the client must let go at the ceiling: the target served {served} bytes of \
             {} before Moira stopped reading, so the cap bounded the model's view and not \
             the heap",
            CHUNK * CHUNKS
        );
    }

    /// A loopback target whose body is far larger than any cap, counting the bytes it actually
    /// hands to hyper. That counter is the observation: it stops climbing when the client stops
    /// reading, so it measures the peak the client was willing to take.
    #[derive(Clone)]
    struct OversizeTarget {
        served: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        chunk: usize,
        chunks: usize,
    }

    async fn serve_oversize(
        axum::extract::State(target): axum::extract::State<OversizeTarget>,
    ) -> axum::response::Response {
        // One allocation, cloned per chunk: `Bytes` is refcounted, so the *target* stays small
        // however large the body it advertises.
        let payload = axum::body::Bytes::from(vec![b'x'; target.chunk]);
        let served = target.served.clone();
        let chunk = target.chunk;
        let body = axum::body::Body::from_stream(futures_util::stream::iter(0..target.chunks).map(
            move |_| {
                served.fetch_add(chunk, std::sync::atomic::Ordering::Relaxed);
                Ok::<_, std::io::Error>(payload.clone())
            },
        ));
        // No `Content-Length`, so this is a chunked body — the shape the finding names, and
        // the one no header pre-check could have caught.
        axum::response::Response::builder()
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(body)
            .expect("oversize response builds")
    }

    async fn start_oversize_target(target: OversizeTarget) -> std::net::SocketAddr {
        let app = axum::Router::new()
            .fallback(axum::routing::any(serve_oversize))
            .with_state(target);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind oversize target");
        let address = listener.local_addr().expect("oversize target address");
        tokio::spawn(async move {
            // The result is ignored because the client disconnects mid-body by design, which
            // is the whole point of this target.
            let _ = axum::serve(listener, app).await;
        });
        address
    }

    /// The importer and this module are two halves of one contract, and they are tested
    /// together here because testing them apart is exactly how they came to disagree: the
    /// importer grew a rename, its own unit test asserted the renamed property was present,
    /// and nothing anywhere asked where the bytes went.
    ///
    /// So this drives the real pipeline end to end — `parse_openapi_document` produces the
    /// `params_schema`, that schema builds the tool unedited, and a loopback target reports
    /// what actually arrived on the wire. Both shapes matter: the ordinary one, which was
    /// always right and must stay right, and the colliding one, where the dispatch was
    /// inverted — the JSON request body serialised into the query string and the scalar
    /// query parameter posted as the JSON body.
    #[tokio::test]
    async fn the_imported_schema_decides_which_argument_is_the_body_and_which_is_a_query() {
        use crate::orchestration::openapi_import::parse_openapi_document;

        let body_schema = json!({
            "required": true,
            "content": {
                "application/json": {
                    "schema": {"type": "object", "properties": {"sku": {"type": "string"}}}
                }
            }
        });
        let document = json!({
            "openapi": "3.0.3",
            "info": {"title": "Dispatch API", "version": "1.0.0"},
            "servers": [{"url": "https://api.example.test"}],
            "paths": {
                // A query parameter named `body` — legal OpenAPI, and the shape that made the
                // importer rename the request body out from under this module.
                "/collide": {
                    "post": {
                        "operationId": "collide",
                        "parameters": [
                            {"name": "body", "in": "query", "required": true,
                             "schema": {"type": "string"}}
                        ],
                        "requestBody": body_schema.clone()
                    }
                },
                "/plain": {
                    "post": {
                        "operationId": "plain",
                        "parameters": [
                            {"name": "page", "in": "query", "schema": {"type": "integer"}}
                        ],
                        "requestBody": body_schema
                    }
                }
            }
        });

        let parsed = parse_openapi_document(&document).expect("the document parses");
        let operation = |path: &str| {
            parsed
                .operations
                .iter()
                .find(|operation| operation.path == path)
                .unwrap_or_else(|| panic!("{path} was imported"))
        };

        let address = start_dispatch_target().await;

        // `request_body` is spelled out rather than read back through
        // `body_property_name`: asking the contract what it expects and then asserting it
        // got it would pass however the contract drifted.
        let collide = call_imported(
            operation("/collide"),
            address,
            json!({"body": "scalar-parameter", "request_body": {"sku": "S-1"}}),
        )
        .await;
        assert_eq!(
            collide.0, "body=scalar-parameter",
            "the parameter named `body` is a query parameter and nothing else"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&collide.1).expect("a JSON request body"),
            json!({"sku": "S-1"}),
            "the request body must be the JSON body, not a query value"
        );

        let plain = call_imported(
            operation("/plain"),
            address,
            json!({"page": 2, "body": {"sku": "S-2"}}),
        )
        .await;
        assert_eq!(plain.0, "page=2");
        assert_eq!(
            serde_json::from_str::<Value>(&plain.1).expect("a JSON request body"),
            json!({"sku": "S-2"}),
            "the uncollided case must be untouched by the collision handling"
        );
    }

    /// Builds a tool from an imported operation, unedited, and calls it against `address`.
    /// Returns the raw query string and the raw request body the target received.
    async fn call_imported(
        operation: &crate::orchestration::ParsedOperation,
        address: std::net::SocketAddr,
        arguments: Value,
    ) -> (String, String) {
        let mut imported = spec(
            &format!("http://{address}{}", operation.path),
            operation.params_schema.clone(),
        );
        imported.method = operation.method;
        imported.allowed_host = "127.0.0.1".to_string();
        // Same dev escape hatch the oversize target above needs, for the same reason.
        imported.outbound_policy.allow_insecure = true;

        let tool = HttpSkillTool::new(imported, reqwest::Client::new()).expect("tool builds");
        let output = tool
            .call_with_extensions(SkillToolArgs(arguments), &ToolCallExtensions::new())
            .await
            .expect("the target answers 200");
        assert_eq!(output.status, 200);
        let received = output.body;
        (
            received["query"].as_str().unwrap_or_default().to_string(),
            received["body"].as_str().unwrap_or_default().to_string(),
        )
    }

    /// Echoes back what it received rather than recording it in shared state: the assertion
    /// is then about this exact call, with no ordering to get wrong.
    async fn serve_dispatch(
        axum::extract::RawQuery(query): axum::extract::RawQuery,
        body: String,
    ) -> axum::response::Response {
        let payload = json!({"query": query.unwrap_or_default(), "body": body}).to_string();
        axum::response::Response::builder()
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(payload))
            .expect("dispatch response builds")
    }

    async fn start_dispatch_target() -> std::net::SocketAddr {
        let app = axum::Router::new().fallback(axum::routing::any(serve_dispatch));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind dispatch target");
        let address = listener.local_addr().expect("dispatch target address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        address
    }
}
