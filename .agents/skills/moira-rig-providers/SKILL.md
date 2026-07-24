---
name: moira-rig-providers
description: Build and configure Rig (rig-core 0.40) provider clients inside Moira's RuntimeFactory. Covers the generic Client<Ext, H> builder surface and its type-state call order, per-provider construction for openai, anthropic, gemini, deepseek, and azure, base-URL normalisation rules including normalize_openai_base_url, Azure api-version and deployment semantics, credential injection through secrecy with the redaction invariants, custom reqwest backends and timeouts, the RuntimeModelHandle enum dispatch pattern, and the end-to-end workflow for adding a new provider variant. Use when adding or changing a ProviderType arm in build_completion_model, altering base_url or credential-type gating, wiring Azure endpoints, choosing between a native and an OpenAI-compatible provider, attaching a custom HTTP client, or reviewing anything under src/orchestration/runtime_factory.rs.
---

# Moira Rig Providers

## Core Rule

Provider clients are constructed in exactly one place: the `match provider.provider_type` in
`RigRuntimeFactory::build_completion_model` (`src/orchestration/runtime_factory.rs`). It takes
resolved config plus a decrypted credential and returns a `RuntimeModelHandle`. Nothing else in
Moira may build a Rig client, and no provider may be reached by hand-rolled HTTP.

Read `.agents/skills/moira-rig-integration/SKILL.md` first — it owns the boundary rules and the
vendored-source verification requirement. This skill owns the per-provider detail.

## Rig 0.40 Client Model

0.40 has **one** client. Every `provider::Client` is a type alias over it
(`rig-core-0.40.0/src/client/mod.rs:173`):

```rust
pub struct Client<Ext = Nothing, H = reqwest::Client> {
    base_url: Arc<str>,
    headers: Arc<HeaderMap>,
    http_client: H,
    ext: Ext,
}
```

Provider differences live in three trait impls: `Provider` (URL joining, per-request customisation),
`Capabilities` (which model types exist), `ProviderBuilder` (default base URL, `finish()` hook).
Consequence: the builder surface below is identical for all five providers Moira supports; only the
extras differ.

### URL joining

`Provider::build_uri` (`client/mod.rs:240`) defaults to plain string concatenation, not `Url::join`:

```rust
let base_url = if base_url.is_empty() || base_url.ends_with('/') {
    base_url.to_string()
} else {
    base_url.to_string() + "/"
};
base_url + path.trim_start_matches('/')
```

Two load-bearing consequences:

- A base URL with a path suffix is **preserved** (`https://gw/x/v1` + `/chat/completions` →
  `https://gw/x/v1/chat/completions`). Gateway prefixes work.
- An **empty** base URL makes the "path" an absolute URL. This is how Azure works — do not "fix" it.

Gemini overrides `build_uri` (`gemini/client.rs:74`); nobody else does.

### Builder surface

`ClientBuilder<Ext, ApiKey = Missing, H = Missing>` (`client/mod.rs:579`) is type-state.

| Method | Where | Notes |
|---|---|---|
| `Client::builder()` | `client/mod.rs:430` | Starts at `ApiKey = Missing`, `H = Missing`. |
| `api_key(impl Into<ApiKey>)` | `client/mod.rs:605` | Only exists on `ClientBuilder<Ext, Missing, H>`. **Call it first, exactly once.** `build()` is unreachable until it is called. |
| `base_url(impl AsRef<str>)` | `client/mod.rs:645` | Replaces the provider default. |
| `http_client(U)` | `client/mod.rs:659` | Advances `H`; selects the concrete-backend `build()`. |
| `http_headers(HeaderMap)` | `client/mod.rs:670` | **Replaces the entire map.** There is no single-header setter. |
| `ext()` / `get_base_url()` | `client/mod.rs:691`, `:696` | Read-only introspection. |
| `build()` | `client/mod.rs:712` (`H = Missing` → substitutes `reqwest::Client`), `:728` (explicit `H`) | Returns `http_client::Result<Client<..>>`. |

`build()` order (`client/mod.rs:728`) — memorise it, header precedence depends on it:

1. `ext_builder.finish(self)?` — provider hook that mutates base URL and headers (Anthropic, Azure).
2. `ExtBuilder::build(&self)?` — constructs the extension (Azure validates the endpoint here).
3. `api_key.into_header()` inserted **only if that header key is not already present**.

So a `http_headers` map containing `authorization` or `x-api-key` silently beats `.api_key(...)`,
while `finish()`-written headers (Anthropic's `anthropic-version`, Azure's auth) overwrite whatever
the caller supplied. Moira does not call `http_headers` today; if you add it, re-verify this order.

### The `build()` error type

`build()` returns `http_client::Result<_>` over `http_client::Error`
(`rig-core-0.40.0/src/http_client/mod.rs:15`). It is a plain `thiserror` enum — **not**
`#[non_exhaustive]` — so adding a match on it is allowed, but a rig-core bump can still add
variants as a breaking change.

Only two variants are reachable from `build()` itself:

- `InvalidHeaderValue(#[from] http::header::InvalidHeaderValue)`, raised when Anthropic's or
  Azure's `finish()` calls `HeaderValue::from_str(api_key)` on a key with illegal bytes. `http`'s
  `InvalidHeaderValue` is a zero-data struct whose `Display` is the fixed string
  `"failed to parse header value"` (`http-1.4.2/src/header/value.rs:29`, `:569`) — it does **not**
  echo the offending bytes.
- `Instance(Box<dyn Error + Send + Sync>)`, which at build time only ever carries Azure's static
  `"Azure client must be provided an endpoint prior to building"` (`azure.rs:135`).

So `safe_config_error` is defence in depth rather than a live leak fix, and it stays lossy anyway:

```rust
fn safe_config_error(provider: &str, err: impl std::fmt::Display) -> AppError {
    AppError::Config(format!("build Rig {provider} client failed: {err}"))
}
```

The genuinely dangerous `Display` output is on the **request** path, not the build path:
`InvalidStatusCodeWithMessage(StatusCode, String)` carries the raw provider response body, and
`Instance` there wraps a `reqwest::Error` whose `Display` appends `" for url ({url})"`
(`reqwest-0.13.4/src/error.rs:278`) — on the Gemini path that URL contains the API key. Those
surface through `CompletionError`, so keep them behind `safe_provider_error_message`; see
`.agents/skills/moira-rig-errors-testing/SKILL.md`.

## Provider Matrix

| | `openai` | `azure` | `anthropic` | `gemini` | `deepseek` |
|---|---|---|---|---|---|
| Moira `ProviderType` | `OpenAi`, `OpenAiCompatible`, `Local` | `AzureOpenAi` | `Anthropic` | `Gemini` | `DeepSeek` |
| Default `BASE_URL` | `https://api.openai.com/v1` (`openai/client.rs:19`) | **`""`** (`azure.rs:115`) | `https://api.anthropic.com` (`anthropic/client.rs:86`) | `https://generativelanguage.googleapis.com` (`gemini/client.rs:13`) | `https://api.deepseek.com` — **no `/v1`** (`deepseek.rs:37`) |
| Auth carrier | `Authorization: Bearer` — `type OpenAIApiKey = BearerAuth` (`openai/client.rs:46`, `client/mod.rs:143`) | `api-key:` or `Authorization: Bearer`, chosen by `AzureOpenAIAuth` (`azure.rs:184`) | `x-api-key:` (`anthropic/client.rs:60`) | **`?key=` in the URL** (`gemini/client.rs:74`) | `Authorization: Bearer` — `type DeepSeekApiKey = BearerAuth` (`deepseek.rs:44`) |
| Extra builder methods | `completions_api()` after `build()` | `azure_endpoint(String)` **required**, `api_version(&str)` | `anthropic_version`, `anthropic_beta(s)` | — | — |
| Moira base-URL policy | `normalize_openai_base_url` | **never set `base_url`** | pass raw | pass raw | `normalize_openai_base_url` |
| Handle type | `openai::completion::CompletionModel` (`openai/completion/mod.rs:1533`) | `azure::CompletionModel` (`azure.rs:555`) | `anthropic::completion::CompletionModel` (`anthropic/completion.rs:1545`) | `gemini::completion::CompletionModel` (`gemini/completion.rs:59`) | `deepseek::CompletionModel` (`deepseek.rs:159`) |
| Wire engine | OpenAI chat-completions | OpenAI chat-completions | Anthropic messages | Gemini generateContent | OpenAI chat-completions |
| Allowed `CredentialType` | `ApiKey`, `BearerToken` | `AzureOpenAi`, `ApiKey` | `ApiKey` | `ApiKey` | `ApiKey` |

Four of the five handle types are type aliases over a generic model, but over **two different**
generics: OpenAI, DeepSeek, and Azure alias
`openai::completion::GenericCompletionModel<Ext, H>` (`openai/completion/mod.rs:1521`), Anthropic
aliases `anthropic::completion::GenericCompletionModel<Ext, T>`. Only Gemini is a distinct struct
(`gemini/completion.rs:59`), because generateContent is neither OpenAI- nor Anthropic-shaped. That
asymmetry is why `RuntimeModelHandle` cannot be collapsed into fewer variants.

## Per-Provider Construction

Every arm follows the same three beats: gate the credential type, build the client, call
`completion_model(model_key)`. `use rig_core::client::CompletionClient;` exists solely to bring
`completion_model` into scope (`rig-core-0.40.0/src/client/completion.rs:9`).

### OpenAI family — `OpenAi | OpenAiCompatible | Local`

```rust
require_credential_type(
    credential.credential_type,
    &[CredentialType::ApiKey, CredentialType::BearerToken],
)?;
let mut builder = openai::Client::builder().api_key(secret.as_str());
if let Some(base_url) = provider.base_url.as_deref() {
    builder = builder.base_url(normalize_openai_base_url(base_url)?);
}
let client = builder
    .build()
    .map_err(|err| safe_config_error("openai-compatible", err))?
    .completions_api();
Ok(RuntimeModelHandle::OpenAi(client.completion_model(model_key)))
```

`openai::Client` is the **Responses API** client (`openai/client.rs:49`). `.completions_api()`
(`openai/client.rs:191`) consumes `self` and swaps only the extension — `base_url`, `headers`, and
`http_client` carry over verbatim. Dropping that call silently moves Moira to `/responses` with a
different request and response shape and a different handle type. `responses_api()`
(`openai/client.rs:251`) is the inverse if Moira ever adopts it.

`Local` and `OpenAiCompatible` reuse this arm deliberately: any gateway speaking chat-completions is
served by the same client. Do not add a bespoke arm for a new OpenAI-compatible vendor.

### Anthropic

```rust
require_credential_type(credential.credential_type, &[CredentialType::ApiKey])?;
let mut builder = anthropic::Client::builder().api_key(secret.as_str());
if let Some(base_url) = provider.base_url.as_deref() {
    builder = builder.base_url(base_url);
}
let client = builder
    .build()
    .map_err(|err| safe_config_error("anthropic", err))?;
Ok(RuntimeModelHandle::Anthropic(client.completion_model(model_key)))
```

Pass the base URL **raw**. Rig normalises it itself in `finish_anthropic_builder`
(`anthropic/client.rs:192`): `normalize_anthropic_base_url` (`:178`) strips a trailing `/`, then a
trailing `/v1/messages`, `/messages`, or `/v1`, and the completion call always posts to the literal
`/v1/messages`. A tenant may therefore configure `https://gateway/anthropic/v1/messages` and it
still works. Running it through `normalize_openai_base_url` would be wrong.

`finish_anthropic_builder` also unconditionally inserts `anthropic-version`
(default `ANTHROPIC_VERSION_LATEST` = `2023-06-01`, `anthropic/completion.rs:37`). Moira must not
inject that header. Beta flags, when needed, go through `anthropic_betas(&[&str])`
(`anthropic/client.rs:160`) or `anthropic_beta(&str)` (`:169`), which are only reachable **after**
`.api_key(...)` because they are defined on `ClientBuilder<H>` with the `ApiKey` slot already
pinned.

### Gemini

```rust
require_credential_type(credential.credential_type, &[CredentialType::ApiKey])?;
let mut builder = gemini::Client::builder().api_key(secret.as_str());
if let Some(base_url) = provider.base_url.as_deref() {
    builder = builder.base_url(base_url);
}
let client = builder
    .build()
    .map_err(|err| safe_config_error("gemini", err))?;
Ok(RuntimeModelHandle::Gemini(client.completion_model(model_key)))
```

**Security-critical:** the Gemini GenerateContent extension carries the API key in the query string,
not in a header (`gemini/client.rs:74`). Every request URI contains the plaintext key. Rig's own
`Debug` redacts `ext.api_key` but does not cover URI strings. Therefore: never log, span-tag, or
propagate a Rig request URI on the Gemini path, and never widen `safe_config_error` /
`safe_provider_error_message` to include URLs. `gemini::InteractionsClient`
(`gemini/client.rs:53`, reachable via `Client::interactions_api()`, `:212`) puts the key in an
`x-goog-api-key` header instead — that is the migration if header-only auth becomes a requirement.

Gemini has no base-URL environment override upstream and no path normalisation. Pass the configured
value raw; the paths appended are `/v1beta/models/{model}:generateContent` and
`:streamGenerateContent`.

### DeepSeek

```rust
require_credential_type(credential.credential_type, &[CredentialType::ApiKey])?;
let mut builder = deepseek::Client::builder().api_key(secret.as_str());
if let Some(base_url) = provider.base_url.as_deref() {
    builder = builder.base_url(normalize_openai_base_url(base_url)?);
}
let client = builder
    .build()
    .map_err(|err| safe_config_error("deepseek", err))?;
Ok(RuntimeModelHandle::DeepSeek(client.completion_model(model_key)))
```

`DeepSeekExt` is zero-sized with no extra builder options. Note the divergence: rig's DeepSeek
default is `https://api.deepseek.com` with **no** `/v1` (`deepseek.rs:142`), while Moira's
`normalize_openai_base_url` force-appends `/v1` to any configured override. That works against
DeepSeek proper (which accepts both) but breaks a DeepSeek-compatible gateway that serves
`/chat/completions` at the root. Treat that as a known limitation, documented below.

### Azure OpenAI

```rust
require_credential_type(
    credential.credential_type,
    &[CredentialType::AzureOpenAi, CredentialType::ApiKey],
)?;
let endpoint = credential
    .config
    .get("endpoint")
    .and_then(Value::as_str)
    .or(provider.base_url.as_deref())
    .ok_or_else(|| {
        AppError::Config("azure_openai provider requires a configured endpoint".to_string())
    })?;
let api_version = credential
    .config
    .get("api_version")
    .and_then(Value::as_str)
    .unwrap_or("2024-10-21");
let client = azure::Client::builder()
    .api_key(azure::AzureOpenAIAuth::ApiKey(secret.to_string()))
    .azure_endpoint(endpoint.to_string())
    .api_version(api_version)
    .build()
    .map_err(|err| safe_config_error("azure_openai", err))?;
Ok(RuntimeModelHandle::AzureOpenAi(client.completion_model(model_key)))
```

Five Azure-specific rules:

1. **Never call `.base_url(...)`.** `AzureExt`'s `BASE_URL` is `""` (`azure.rs:115`) so that
   `build_uri`'s empty-base-URL branch lets `completion_path` emit an absolute URL. Setting a base
   URL breaks every request path.
2. **The endpoint lives in the extension**, supplied by `azure_endpoint(String)` (`azure.rs:173`),
   shaped `https://{resource}.openai.azure.com`. It is mandatory: `ProviderBuilder::build` returns
   `http_client::Error::Instance("Azure client must be provided an endpoint prior to building")`
   (`azure.rs:134`) when it is absent.
3. **Auth must be constructed explicitly.** `AzureOpenAIAuth::ApiKey(String)` writes an `api-key:`
   header in `finish()`; `AzureOpenAIAuth::Token(String)` writes `Authorization: Bearer`
   (`azure.rs:140`). `impl<S> From<S> for AzureOpenAIAuth` (`azure.rs:200`) coerces a bare string to
   **`Token`**, so `.api_key(secret.as_str())` would silently produce bearer auth and 401 against a
   key-authenticated resource. Always name the variant. Moira only constructs `ApiKey`; Entra/AD
   token auth is unimplemented.
4. **`.api_key(...)` must come first, and the type system enforces it.** `azure_endpoint`
   (`azure.rs:173`) is defined on `client::ClientBuilder<AzureExtBuilder, AzureOpenAIAuth, H>` and
   `api_version` (`azure.rs:164`) on the same alias, so neither is callable while the `ApiKey` slot
   is still `Missing`. `azure_endpoint` returns that same alias, so `azure_endpoint` and
   `api_version` may be called in either order relative to each other; only `api_key` is pinned to
   the front.
5. **`model_key` is the deployment name, not a model name.** `completion_path`
   (`azure.rs:568`) formats
   `{endpoint}/openai/deployments/{model}/chat/completions?api-version={api_version}`, and the same
   string is also sent as the JSON `model` field. The URL is pinned to the handle's model; a
   per-request `model` override changes only the body. Azure's model constants in `azure.rs` are
   therefore near-useless — configure the deployment name in `provider_models.model_key`.

`api_version` default `"2024-10-21"` matches rig's `DEFAULT_API_VERSION` (`azure.rs:48`). Preview
features need an explicit `credential.config.api_version` such as `"2024-10-01-preview"`.
`VERIFY_PATH` is `""` (`azure.rs:93`) — Azure auth cannot be verified without spending tokens, so
there is no cheap health probe.

## Base URL Decision Table

| Situation | Action |
|---|---|
| `ProviderType::OpenAi`, `OpenAiCompatible`, `Local`, `DeepSeek` | `normalize_openai_base_url(base_url)?` |
| `ProviderType::Anthropic`, `Gemini` | pass raw — rig normalises Anthropic, Gemini needs the host only |
| `ProviderType::AzureOpenAi` | do not set `base_url`; route it into `azure_endpoint` |
| `provider.base_url` is `None` | omit the call entirely; rig's `BASE_URL` default applies |
| New provider with its own path convention | pass raw unless you can prove the provider's path shape matches OpenAI's `/v1` convention |

`normalize_openai_base_url` (`src/orchestration/resolver.rs`) is the only URL rewriting Moira does:

```rust
pub fn normalize_openai_base_url(base_url: &str) -> Result<String, AppError> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let parsed = url::Url::parse(trimmed)
        .map_err(|err| AppError::BadRequest(format!("invalid provider base_url: {err}")))?;
    if parsed.path().trim_end_matches('/').ends_with("/v1") {
        Ok(trimmed.to_string())
    } else {
        Ok(format!("{trimmed}/v1"))
    }
}
```

It validates the URL (bad input → `AppError::BadRequest`) and force-appends `/v1` when absent.
Known limitation to state rather than paper over: a gateway that serves `/chat/completions` at the
root cannot be configured. If a tenant needs that, change `normalize_openai_base_url` behind an
explicit provider setting and add regression coverage next to the existing test in `resolver.rs` —
do not special-case it inside `build_completion_model`.

## Credential Injection and Redaction

Flow: DB row → `EncryptedSecret` → AES-GCM decrypt with AAD → JSON payload → field extraction →
`secrecy::SecretString` → **one** `expose_secret()` at the provider builder.

```rust
let secret = credential.secret.expose_secret();
```

That single line in `build_completion_model` is the only `expose_secret()` in the Rig path. Keep it
that way: do not clone the exposed value into a struct, a log field, a span attribute, an error
message, or a cache key.

Credential-type gating is a hard gate, mirrored in two places that must stay in sync:

```rust
fn require_credential_type(
    actual: CredentialType,
    allowed: &[CredentialType],
) -> Result<(), AppError> {
    if allowed.contains(&actual) {
        Ok(())
    } else {
        Err(AppError::Config(format!(
            "credential type {actual:?} is not supported by this provider"
        )))
    }
}
```

The mirror is `supported_credential_types(provider_type)` in `src/application/execution.rs`, used
for pre-flight credential selection; the unit test
`provider_credential_types_match_runtime_factory_support` pins the two together. Changing one arm
without the other is a review-blocking defect.

Which JSON field the secret comes from is decided earlier, in
`secret_from_credential_payload` (`src/application/execution.rs`): `ApiKey` and `AzureOpenAi` →
`"api_key"`, `BearerToken` → `"bearer_token"`, `Oauth2` → `"access_token"`; `BasicAuth`,
`CustomHeaders`, and `ServiceAccount` are rejected as non-executable. A new provider that needs a
different shape needs a change there, not in the factory.

### What must never be logged, serialised, or returned

- Plaintext API keys, bearer tokens, Azure keys, decrypted credential payloads.
- Rig request URIs — the Gemini key rides in the query string.
- Raw provider response bodies or raw `http_client::Error` / `CompletionError` `Display` output to
  API clients. Use `safe_config_error` and `safe_provider_error_message`.
- Internal prompts, preambles, and protected instructions.

Three redaction layers already exist and must be preserved:

1. Hand-written `Debug for RuntimeModelHandle` printing `RuntimeModelHandle::OpenAi(<redacted>)`
   per variant. **Never `#[derive(Debug)]` on this enum** — a new variant needs a new arm.
2. Hand-written `Debug for ResolvedCredential` (`src/domain/runtime.rs`) redacting `secret` and
   passing `config` through `redact_credential_config`.
3. Sanitised error text at the boundary. Rig's own `Debug for Client` (`client/mod.rs:188`) filters
   `AUTHORIZATION` and `*api-key*` headers but still prints the base URL — necessary, not
   sufficient.

## Custom HTTP Clients and Timeouts

`build_completion_model` accepts `ProviderRuntimePolicyRecord` and ignores it (`_policy`). Nothing
in the transport is policy-derived today:

- `request_timeout_ms` is clamped into the effective attempt timeout
  (`src/application/execution.rs:1692`) and enforced by `tokio::time::timeout` around the whole
  attempt (`:502`), not by the HTTP client.
- `stream_idle_timeout_ms` is enforced per streamed item, also by `tokio::time::timeout` (`:1479`).
- `connect_timeout_ms` is **not enforced anywhere** — it is stored, versioned, and read only by
  test fixtures. Do not claim otherwise; wiring it is the open work described below.

If you wire the policy into the transport, the supported mechanism is `ClientBuilder::http_client`
(`client/mod.rs:659`). `impl HttpClientExt for reqwest::Client` exists
(`http_client/mod.rs:155`), and `Cargo.lock` resolves a **single** `reqwest 0.13.4` shared by Moira
and rig-core — so Moira's `reqwest::Client` is the same type, and passing it keeps `H =
reqwest::Client`, which is the default generic in every `RuntimeModelHandle` variant. The enum does
not change:

```rust
let http = reqwest::Client::builder()
    .connect_timeout(Duration::from_millis(policy.connect_timeout_ms as u64))
    .build()
    .map_err(|err| safe_config_error("openai-compatible", err))?;

let client = openai::Client::builder()
    .api_key(secret.as_str())
    .http_client(http)
    .build()
    .map_err(|err| safe_config_error("openai-compatible", err))?
    .completions_api();
```

Rules if you do this:

- Use `connect_timeout` (`reqwest-0.13.4/src/async_impl/client.rs:1469`) only. `ClientBuilder::
  timeout` (`:1444`) is documented as "applied from when the request starts connecting until the
  response body has finished", so it would kill long streams. Whole-request and idle deadlines stay
  in Moira's `tokio::time::timeout` and `stream_idle_timeout_ms`. `read_timeout` (`:1456`) is the
  only other candidate and duplicates the existing stream idle timeout — do not add both.
- `runtime_policy_version` is already part of `RuntimeCacheKey`
  (`src/orchestration/controls.rs`), so policy-derived transports invalidate correctly. Verify that
  before shipping.
- `reqwest_middleware::ClientWithMiddleware` also implements `HttpClientExt`
  (`http_client/mod.rs:289`), but that impl is behind rig-core's optional `reqwest-middleware`
  feature (`rig-core-0.40.0/Cargo.toml:59`), which Moira does not enable, and the crate is not a
  Moira dependency. Adding it is a dependency and feature decision, not a factory change.

## Choosing Native vs OpenAI-Compatible

| Question | Answer |
|---|---|
| Vendor speaks OpenAI chat-completions on a custom host | `ProviderType::OpenAiCompatible` with `base_url`. No code change. |
| Self-hosted model server (vLLM, llama.cpp, Ollama shim) speaking chat-completions | `ProviderType::Local`. Same arm. No code change. |
| Vendor has a dedicated rig-core 0.40 provider module with its own wire format | New `ProviderType` variant using that module. Follow the workflow below. |
| Vendor has a dedicated rig module but is OpenAI-compatible on the wire | Still prefer the dedicated module — it carries provider-specific `finalize_request_body` fixes (DeepSeek is the example) that `OpenAiCompatible` will not apply. |
| Vendor has no rig-core 0.40 provider module | `AppError::Config`, as `ProviderType::Custom` does today. Do not hand-roll HTTP; contribute upstream. |

## Workflow: Adding a New Provider

Do these in order. Steps 2–6 are compiler-guided: `ProviderType` is matched exhaustively in several
places, so `cargo build` enumerates the remaining work after step 2.

1. **Verify the Rig module exists in 0.40.0 before anything else.** Confirm, by reading the vendored
   source, the `Client` alias path, the `CompletionModel` alias or struct path, the `BASE_URL`
   default, the auth carrier, any mandatory extra builder method, and whether `build_uri` is
   overridden. If any of these cannot be found, stop — the provider is not supportable.
   ```bash
   RIG=/Users/nalhide/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rig-core-0.40.0
   # some providers are a directory (openai/, anthropic/, gemini/), others a single file (deepseek.rs, azure.rs)
   ls "$RIG/src/providers"
   rg -n 'pub type Client|pub type CompletionModel|pub struct CompletionModel|const BASE_URL|const VERIFY_PATH|fn build_uri|type ApiKey' \
     "$RIG/src/providers/<name>" "$RIG/src/providers/<name>.rs"
   ```
2. **Migration.** Add a **new** append-only SQL migration extending the `providers.provider_type`
   check constraint (originally in `migrations/0003_security_foundation.sql`). Never edit a
   committed migration.
3. **Domain.** Add the variant to `ProviderType` in `src/domain/admin.rs`. It is
   `#[serde(rename_all = "snake_case")]` and `ToSchema`, so the variant name is both the wire value
   and part of the OpenAPI enum.
4. **Row mapping.** Add both directions in `src/infra/pg_rows.rs`; the string must match the
   migration's check-constraint literal exactly.
5. **Credential gate.** Add the arm to `supported_credential_types` in
   `src/application/execution.rs` and extend
   `provider_credential_types_match_runtime_factory_support`. If the credential payload uses a new
   field name, extend `secret_from_credential_payload` too.
6. **Factory.** In `src/orchestration/runtime_factory.rs`:
   - add the `RuntimeModelHandle` variant with the exact Rig type path from step 1;
   - add the matching `Debug` arm printing `<redacted>`;
   - add arms to the two per-variant matches, `completion` and `start_stream` — each delegates to
     the existing generic free function, no new logic. `stream` is a wrapper over `start_stream`
     and needs no arm;
   - add the `build_completion_model` arm: `require_credential_type(...)`, builder chain, base-URL
     policy from the decision table, `safe_config_error("<name>", err)`, `completion_model(model_key)`.
   Check that the provider's `CompletionModel` satisfies the streaming bounds
   (`M::StreamingResponse: Clone + Unpin + GetTokenUsage + Serialize + Send + 'static`); if it does
   not, see `.agents/skills/moira-rig-streaming/SKILL.md` before proceeding.
7. **Wire-shape review.** Read the provider's `OpenAICompatibleProvider` /
   `AnthropicCompatibleProvider` impl or its hand-rolled request builder and note anything that
   changes request semantics: mandatory fields, `additional_params` keys that are consumed rather
   than flattened, dropped `output_schema`, silently ignored `temperature`/`max_tokens`. Anything
   that affects the request belongs in `.agents/skills/moira-rig-completions/SKILL.md`; anything
   that affects error mapping belongs in `.agents/skills/moira-rig-errors-testing/SKILL.md`.
8. **API contract.** `ProviderType` appears in admin DTOs and in `src/domain/public.rs`, so the
   OpenAPI enum changes. Follow `.agents/skills/moira-openapi/SKILL.md`.
9. **Tests.** Add the credential-type unit test. End-to-end coverage against
   `tests/support/mock_openai.rs` is only meaningful for providers on the OpenAI chat-completions
   engine; for a native wire format, add a scripted server for that shape rather than asserting
   against the OpenAI one. Do not mock Rig types.
10. **Docs.** Update `docs/provider-runtime-factory.md`, `docs/provider-management.md`, and
    `docs/rig-integration.md`.
11. **Validate** with the commands below.

## Pitfalls

- Omitting `.completions_api()` on the OpenAI arm compiles only if the handle type is also changed;
  if someone "fixes" the type instead, Moira silently moves to the Responses API.
- `.api_key(bare_str)` on Azure yields bearer `Token` auth via the blanket `From<S>` impl. Always
  name `AzureOpenAIAuth::ApiKey`.
- Calling `.base_url(...)` on Azure, or `normalize_openai_base_url` on Anthropic or Gemini, produces
  a wrong URL with no compile error.
- `api_key` is only callable while the `ApiKey` slot is `Missing`; provider-specific builder methods
  (`azure_endpoint`, `api_version`, `anthropic_version`, `anthropic_beta`) require the slot already
  pinned. Order is not stylistic.
- `http_headers` replaces the whole `HeaderMap` and interacts with the `finish()` → `build()` header
  precedence described above. Prefer provider-specific builder methods over raw headers.
- Rig's `Client` `Debug` prints the base URL and Gemini's URIs contain the API key. Never log Rig
  request URIs.
- A new `RuntimeModelHandle` variant without a `Debug` arm is impossible today (the impl is manual
  and exhaustive) — keep it manual so that stays true.
- `ProviderType::Custom` returning `AppError::Config` is intentional, not a gap to fill with a
  hand-rolled client.
- `src/orchestration/executor.rs` builds an `openai::CompletionsClient` only to read `base_url()`
  (`executor.rs:21`, `:46`, `:81`) and then issues raw `reqwest` calls with `bearer_auth` over a
  plaintext key. It is legacy V1: its only consumer, `src/http/chat.rs`, is not declared in
  `src/http/mod.rs`, so it is not even compiled. Never extend it and never copy its pattern.

## Validation

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Provider changes must additionally prove: the credential type gate rejects unsupported types without
leaking the secret, `supported_credential_types` and `require_credential_type` agree, the base-URL
policy for the new provider has a unit test next to `normalize_openai_base_url`'s, and no test
output or surfaced error message contains a key, a raw provider body, or a request URI. When
database behaviour changes, validate the new migration against the local pgvector Postgres
container.

Related: `.agents/skills/moira-rig-integration/SKILL.md`,
`.agents/skills/moira-rig-completions/SKILL.md`, `.agents/skills/moira-rig-streaming/SKILL.md`,
`.agents/skills/moira-rig-errors-testing/SKILL.md`, `skills/moira-project-structure/SKILL.md`.
