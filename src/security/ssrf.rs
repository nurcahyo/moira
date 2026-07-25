//! SSRF hardening for outbound JWKS fetches (plan 03 / finding P1-2).
//!
//! `jwks_url` is **admin-configured** (`POST /api/v1/admin/jwt-issuers`) and
//! deployment-configured (`MOIRA_AUTH__ADMIN__JWKS_URL` /
//! `MOIRA_AUTH__CALLER__JWKS_URL`). Without the checks in this module a typo or a
//! compromised admin credential turns Moira's outbound HTTP client into an SSRF
//! probe against `http://169.254.169.254/latest/meta-data/`, `http://localhost:6379`,
//! or any RFC1918 service reachable from the pod.
//!
//! The IP classification ([`is_denied_ip`]) is deliberately a **pure function** with
//! no network I/O so every denied range is unit-testable in isolation.
//!
//! Residual risk (accepted, documented in `plans/03-security-hardening.md`): DNS is
//! resolved and validated, then `reqwest` resolves again when it connects, leaving a
//! narrow TOCTOU rebinding window. Closing it requires pinning the validated address
//! through a custom `reqwest::dns::Resolve`, which is a larger change; `jwks_url` is
//! an admin-configured rather than attacker-configured input surface.

use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use futures_util::StreamExt;
use jsonwebtoken::jwk::JwkSet;
use reqwest::{Client, header::CONTENT_TYPE};
use tokio::net::lookup_host;
use url::{Host, Url};

use crate::{config::JwksFetchSettings, error::AppError};

/// Content types a JWKS document is allowed to be served as. Some IdPs use the
/// registered `application/jwk-set+json`; most use plain `application/json`.
/// Rejecting anything else stops Moira from feeding an arbitrary internal HTTP
/// response body into a JSON parser.
const ALLOWED_JWKS_CONTENT_TYPES: [&str; 2] = ["application/json", "application/jwk-set+json"];

/// Client-visible message for a registration-time rejection. Deliberately free of
/// the resolved address and the denial reason — see [`JwksFetchError::into_unauthorized`].
const JWKS_REJECTED_MESSAGE: &str = "The JWKS URL was rejected by the server's security policy.";

/// Client-visible message when a lazy JWKS fetch fails during token verification.
/// A caller must not learn *why*, or the auth path becomes an SSRF oracle.
const JWKS_UNAUTHORIZED_MESSAGE: &str = "unable to verify the token signing keys";

/// Why a JWKS fetch was refused. Recorded in the audit log and in server-side
/// tracing; **never** returned to the caller on the verification path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JwksDenialReason {
    /// The configured value is not a parseable absolute URL.
    Url,
    /// Scheme was not `https` and the dev override is off.
    Scheme,
    /// The URL carries no host component at all.
    Host,
    /// DNS resolution failed or returned no addresses.
    Resolution,
    /// A resolved address falls inside a denied range.
    IpRange,
    /// Connection/TLS/read failure.
    Transport,
    /// Upstream returned a non-2xx status.
    Status,
    /// Missing or unsupported `Content-Type`.
    ContentType,
    /// Body exceeded `max_response_bytes`.
    Size,
    /// The fetch exceeded `timeout_ms`.
    Timeout,
    /// The body was not a valid JWKS document.
    Parse,
}

impl JwksDenialReason {
    /// Stable, machine-filterable token for audit metadata and log fields.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Url => "url",
            Self::Scheme => "scheme",
            Self::Host => "host",
            Self::Resolution => "resolution",
            Self::IpRange => "ip_range",
            Self::Transport => "transport",
            Self::Status => "status",
            Self::ContentType => "content_type",
            Self::Size => "size",
            Self::Timeout => "timeout",
            Self::Parse => "parse",
        }
    }
}

/// A refused JWKS fetch. `detail` is **server-side only**: it can name the resolved
/// internal address, which is exactly what must not reach a caller.
#[derive(Debug, Clone)]
pub struct JwksFetchError {
    reason: JwksDenialReason,
    detail: String,
}

impl JwksFetchError {
    pub(crate) fn new(reason: JwksDenialReason, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }

    pub fn reason(&self) -> JwksDenialReason {
        self.reason
    }

    /// Server-side detail. Log it, audit it, never serialise it into a response.
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Registration-time (`POST /api/v1/admin/jwt-issuers`) rejection: the bad value
    /// is the admin's own input, so a `400` with the catalogued
    /// `moira.error.jwks_url_rejected` code tells them to fix the request.
    pub fn into_registration_error(&self) -> AppError {
        AppError::coded(
            axum::http::StatusCode::BAD_REQUEST,
            "jwks_url_rejected",
            JWKS_REJECTED_MESSAGE,
        )
    }

    /// Verification-time rejection: the caller merely presented a JWT that triggered
    /// a lazy fetch. They get the generic catalogued `unauthorized` error and learn
    /// nothing about the scheme/range/size/content-type decision, so the auth path
    /// cannot be used as an SSRF oracle.
    pub fn into_unauthorized(&self) -> AppError {
        AppError::Unauthorized(JWKS_UNAUTHORIZED_MESSAGE.to_string())
    }
}

impl fmt::Display for JwksFetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.reason.as_str(), self.detail)
    }
}

impl std::error::Error for JwksFetchError {}

/// Pure IP-range classification. `true` means "Moira must not connect to this".
///
/// Covers loopback, link-local (including every cloud metadata endpoint, which all
/// live in `169.254.0.0/16`), RFC1918 private space, IPv6 unique-local, unspecified,
/// multicast, broadcast, CGNAT, benchmarking, and reserved space — plus the
/// IPv4-in-IPv6 encodings of all of the above.
pub fn is_denied_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_denied_ipv4(v4),
        IpAddr::V6(v6) => is_denied_ipv6(v6),
    }
}

fn is_denied_ipv4(ip: Ipv4Addr) -> bool {
    // `is_unspecified`, `is_loopback`, `is_private`, `is_link_local`, `is_multicast`
    // and `is_broadcast` are all stable on the pinned toolchain (1.97).
    if ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
    {
        return true;
    }

    let [a, b, c, _] = ip.octets();
    // 100.64.0.0/10 — carrier-grade NAT, routable inside many private networks.
    if a == 100 && (64..=127).contains(&b) {
        return true;
    }
    // 192.0.0.0/24 — IETF protocol assignments (includes 192.0.0.8 etc.).
    if a == 192 && b == 0 && c == 0 {
        return true;
    }
    // 198.18.0.0/15 — benchmarking.
    if a == 198 && (b == 18 || b == 19) {
        return true;
    }
    // 240.0.0.0/4 — reserved for future use.
    if a >= 240 {
        return true;
    }
    false
}

fn is_denied_ipv6(ip: Ipv6Addr) -> bool {
    // Checked before the IPv4 unwrap below so `::` and `::1` are classified as the
    // v6 addresses they are rather than as 0.0.0.0 / 0.0.0.1.
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return true;
    }

    // Closes the `::ffff:169.254.169.254` bypass: unwrap the embedded v4 address and
    // re-run the full v4 rule set on it. `to_ipv4` also covers the deprecated
    // IPv4-compatible `::a.b.c.d` form.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_denied_ipv4(v4);
    }
    if let Some(v4) = ip.to_ipv4() {
        return is_denied_ipv4(v4);
    }

    let segments = ip.segments();
    // fc00::/7 — unique local addresses. `Ipv6Addr::is_unique_local` is still
    // unstable on the pinned toolchain, so the prefix test is written out.
    if segments[0] & 0xfe00 == 0xfc00 {
        return true;
    }
    // fe80::/10 — link-local unicast. `Ipv6Addr::is_unicast_link_local` is likewise
    // unstable on 1.97.
    if segments[0] & 0xffc0 == 0xfe80 {
        return true;
    }
    false
}

/// Parses and validates a JWKS URL without fetching it.
///
/// Order matters: scheme first (cheap, no I/O), then host classification. A hostname
/// is resolved with [`tokio::net::lookup_host`] and **every** returned address is
/// classified — a hostname can resolve to several, and a single denied address is
/// enough to refuse the whole URL.
///
/// When `allow_insecure_dev_urls` is set the rejections are skipped entirely and a
/// `WARN` event is emitted so it is visible in logs that the protection is off.
pub async fn validate_jwks_url(
    raw_url: &str,
    settings: &JwksFetchSettings,
) -> Result<Url, JwksFetchError> {
    let url = Url::parse(raw_url).map_err(|err| {
        JwksFetchError::new(
            JwksDenialReason::Url,
            format!("jwks url is not a valid absolute url: {err}"),
        )
    })?;

    if settings.allow_insecure_dev_urls {
        tracing::warn!(
            jwks_url = %url,
            "JWKS SSRF protection is disabled by auth.jwks.allow_insecure_dev_urls; \
             scheme and address-range checks were skipped"
        );
        return Ok(url);
    }

    if url.scheme() != "https" {
        return Err(JwksFetchError::new(
            JwksDenialReason::Scheme,
            format!("jwks url scheme '{}' is not https", url.scheme()),
        ));
    }

    let host = url.host().ok_or_else(|| {
        JwksFetchError::new(JwksDenialReason::Host, "jwks url has no host component")
    })?;

    match host {
        // The `url` crate parses spec-compliant IPv4 literals — including the
        // decimal (`2130706433`) and hex (`0x7f.0.0.1`) forms — into `Host::Ipv4`,
        // so those bypasses are classified here rather than falling through to DNS.
        Host::Ipv4(v4) => reject_if_denied(IpAddr::V4(v4))?,
        Host::Ipv6(v6) => reject_if_denied(IpAddr::V6(v6))?,
        Host::Domain(domain) => {
            let port = url.port_or_known_default().unwrap_or(443);
            let resolved: Vec<SocketAddr> = lookup_host((domain, port))
                .await
                .map_err(|err| {
                    JwksFetchError::new(
                        JwksDenialReason::Resolution,
                        format!("jwks host '{domain}' could not be resolved: {err}"),
                    )
                })?
                .collect();
            if resolved.is_empty() {
                return Err(JwksFetchError::new(
                    JwksDenialReason::Resolution,
                    format!("jwks host '{domain}' resolved to no addresses"),
                ));
            }
            for address in resolved {
                reject_if_denied(address.ip())?;
            }
        }
    }

    Ok(url)
}

fn reject_if_denied(ip: IpAddr) -> Result<(), JwksFetchError> {
    if is_denied_ip(ip) {
        return Err(JwksFetchError::new(
            JwksDenialReason::IpRange,
            format!("jwks url resolves to denied address {ip}"),
        ));
    }
    Ok(())
}

/// SSRF-hardened JWKS fetch: validated URL, explicit timeout, streamed size cap,
/// content-type check, then parse.
pub async fn fetch_jwks_hardened(
    http: &Client,
    raw_url: &str,
    settings: &JwksFetchSettings,
) -> Result<JwkSet, JwksFetchError> {
    let url = validate_jwks_url(raw_url, settings).await?;
    let budget = Duration::from_millis(settings.timeout_ms.max(1));

    // Belt and braces: `RequestBuilder::timeout` bounds the reqwest request, and the
    // outer `tokio::time::timeout` bounds the whole operation including the streamed
    // body read, so a trickling upstream cannot hold the connection open forever.
    match tokio::time::timeout(budget, fetch_validated(http, url, settings, budget)).await {
        Ok(result) => result,
        Err(_) => Err(JwksFetchError::new(
            JwksDenialReason::Timeout,
            format!("jwks fetch exceeded {} ms", settings.timeout_ms),
        )),
    }
}

async fn fetch_validated(
    http: &Client,
    url: Url,
    settings: &JwksFetchSettings,
    budget: Duration,
) -> Result<JwkSet, JwksFetchError> {
    let response = http
        .get(url.clone())
        .timeout(budget)
        .send()
        .await
        .map_err(|err| {
            if err.is_timeout() {
                JwksFetchError::new(
                    JwksDenialReason::Timeout,
                    format!("jwks request timed out: {err}"),
                )
            } else {
                JwksFetchError::new(
                    JwksDenialReason::Transport,
                    format!("jwks request failed: {err}"),
                )
            }
        })?;

    let status = response.status();
    if !status.is_success() {
        return Err(JwksFetchError::new(
            JwksDenialReason::Status,
            format!("jwks endpoint returned status {status}"),
        ));
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| {
            JwksFetchError::new(
                JwksDenialReason::ContentType,
                "jwks response has no content-type header",
            )
        })?;
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if !ALLOWED_JWKS_CONTENT_TYPES.contains(&essence.as_str()) {
        return Err(JwksFetchError::new(
            JwksDenialReason::ContentType,
            format!("jwks response content-type '{essence}' is not a JSON media type"),
        ));
    }

    let cap = settings.max_response_bytes;
    // A declared `Content-Length` over the cap is a cheap early reject. It is *not*
    // authoritative — the header can be absent (chunked) or simply lie — so the
    // streaming counter below is what actually enforces the limit.
    if let Some(declared) = response.content_length()
        && declared > cap as u64
    {
        return Err(JwksFetchError::new(
            JwksDenialReason::Size,
            format!("jwks response declares {declared} bytes, cap is {cap}"),
        ));
    }

    // Deliberately not `.json()`: that buffers the whole body with no ceiling, which
    // would make `max_response_bytes` decorative. Returning early drops the stream,
    // which closes the connection mid-transfer.
    let mut stream = response.bytes_stream();
    let mut buffered: Vec<u8> = Vec::with_capacity(cap.min(8 * 1024));
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| {
            JwksFetchError::new(
                JwksDenialReason::Transport,
                format!("jwks response body read failed: {err}"),
            )
        })?;
        if buffered.len().saturating_add(chunk.len()) > cap {
            return Err(JwksFetchError::new(
                JwksDenialReason::Size,
                format!("jwks response exceeded the {cap} byte cap"),
            ));
        }
        buffered.extend_from_slice(&chunk);
    }

    serde_json::from_slice::<JwkSet>(&buffered).map_err(|err| {
        JwksFetchError::new(
            JwksDenialReason::Parse,
            format!("jwks response is not a valid JWKS document: {err}"),
        )
    })
}

/// A loopback JWKS stub used by the unit tests in this module and by the JWKS-cache
/// tests in `auth.rs`. No new dev-dependency: a bare `tokio::net::TcpListener`
/// speaking just enough HTTP/1.1, mirroring the pattern already in
/// `tests/support/mod.rs`.
#[cfg(test)]
pub(crate) mod test_stub {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::Notify,
    };

    pub(crate) const EMPTY_JWKS: &str = r#"{"keys":[]}"#;

    /// What the stub should send. `hold` (when set) is awaited before the *first*
    /// response is written, which is the acknowledgement gate the singleflight test
    /// uses instead of a `sleep`.
    pub(crate) struct StubPlan {
        pub status_line: &'static str,
        pub content_type: &'static str,
        pub body: String,
        /// Bytes to advertise in `Content-Length`; defaults to `body.len()`. Set it
        /// higher than the body to exercise a lying header.
        pub declared_length: Option<usize>,
        /// Stream the body with `Transfer-Encoding: chunked` and **no**
        /// `Content-Length`, so the size cap can only be enforced by the streaming
        /// counter — the case a header-only check cannot catch.
        pub chunked: bool,
        /// After the first request, answer with `500` instead — models an IdP that
        /// was reachable once and then broke.
        pub fail_after_first: bool,
        pub hold: Option<Arc<Notify>>,
        /// Signalled once the stub has accepted the first request.
        pub arrived: Option<Arc<Notify>>,
    }

    impl StubPlan {
        pub fn json(body: &str) -> Self {
            Self {
                status_line: "HTTP/1.1 200 OK",
                content_type: "application/json",
                body: body.to_string(),
                declared_length: None,
                chunked: false,
                fail_after_first: false,
                hold: None,
                arrived: None,
            }
        }
    }

    pub(crate) struct Stub {
        pub url: String,
        pub hits: Arc<AtomicUsize>,
    }

    pub(crate) async fn spawn(plan: StubPlan) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the jwks stub must bind a loopback port");
        let address = listener.local_addr().expect("stub must report its address");
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let seen = counter.fetch_add(1, Ordering::SeqCst);

                if seen == 0 {
                    if let Some(arrived) = plan.arrived.as_ref() {
                        arrived.notify_waiters();
                    }
                    if let Some(hold) = plan.hold.as_ref() {
                        hold.notified().await;
                    }
                }

                // Drain the request line/headers so the client's write completes.
                let mut scratch = [0_u8; 2048];
                let _ = socket.read(&mut scratch).await;

                if plan.fail_after_first && seen > 0 {
                    let _ = socket
                        .write_all(
                            b"HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                        )
                        .await;
                    let _ = socket.flush().await;
                    continue;
                }

                if plan.chunked {
                    let head = format!(
                        "{}\r\nContent-Type: {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                        plan.status_line, plan.content_type
                    );
                    if socket.write_all(head.as_bytes()).await.is_err() {
                        continue;
                    }
                    for slice in plan.body.as_bytes().chunks(1024) {
                        let framed = format!(
                            "{:x}\r\n{}\r\n",
                            slice.len(),
                            String::from_utf8_lossy(slice)
                        );
                        // The client aborts mid-body once the cap is crossed, so a
                        // write error here is the expected outcome, not a failure.
                        if socket.write_all(framed.as_bytes()).await.is_err() {
                            break;
                        }
                        let _ = socket.flush().await;
                    }
                    let _ = socket.write_all(b"0\r\n\r\n").await;
                    let _ = socket.flush().await;
                    continue;
                }

                let declared = plan.declared_length.unwrap_or(plan.body.len());
                let response = format!(
                    "{}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    plan.status_line, plan.content_type, declared, plan.body
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });

        Stub {
            url: format!("http://{address}/jwks"),
            hits,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{test_stub::*, *};

    fn ip(value: &str) -> IpAddr {
        value.parse().expect("test address must parse")
    }

    fn hardened() -> JwksFetchSettings {
        JwksFetchSettings::default()
    }

    fn dev_override() -> JwksFetchSettings {
        JwksFetchSettings {
            allow_insecure_dev_urls: true,
            ..JwksFetchSettings::default()
        }
    }

    #[test]
    fn loopback_addresses_are_denied() {
        assert!(is_denied_ip(ip("127.0.0.1")));
        assert!(is_denied_ip(ip("127.1.2.3")));
        assert!(is_denied_ip(ip("::1")));
    }

    #[test]
    fn rfc1918_private_ranges_are_denied() {
        assert!(is_denied_ip(ip("10.0.0.1")));
        assert!(is_denied_ip(ip("172.16.0.1")));
        assert!(is_denied_ip(ip("172.31.255.254")));
        assert!(is_denied_ip(ip("192.168.1.1")));
    }

    #[test]
    fn link_local_and_cloud_metadata_addresses_are_denied() {
        assert!(is_denied_ip(ip("169.254.0.1")));
        assert!(
            is_denied_ip(ip("169.254.169.254")),
            "the AWS/GCP/Azure/Alibaba metadata endpoint must be denied"
        );
    }

    #[test]
    fn ipv6_unique_local_and_link_local_are_denied() {
        assert!(is_denied_ip(ip("fc00::1")));
        assert!(is_denied_ip(ip("fd12:3456::1")));
        assert!(is_denied_ip(ip("fe80::1")));
        assert!(is_denied_ip(ip("febf::1")), "fe80::/10 covers up to febf");
    }

    #[test]
    fn ipv4_mapped_ipv6_metadata_address_is_denied() {
        assert!(
            is_denied_ip(ip("::ffff:169.254.169.254")),
            "the ipv4-mapped form must not bypass the link-local rule"
        );
        assert!(is_denied_ip(ip("::ffff:127.0.0.1")));
        assert!(is_denied_ip(ip("::ffff:10.0.0.1")));
    }

    #[test]
    fn unspecified_and_multicast_addresses_are_denied() {
        assert!(is_denied_ip(ip("0.0.0.0")));
        assert!(is_denied_ip(ip("::")));
        assert!(is_denied_ip(ip("224.0.0.1")));
        assert!(is_denied_ip(ip("ff02::1")));
        assert!(is_denied_ip(ip("255.255.255.255")));
    }

    #[test]
    fn shared_benchmark_and_reserved_ranges_are_denied() {
        assert!(is_denied_ip(ip("100.64.0.1")), "carrier-grade NAT");
        assert!(is_denied_ip(ip("198.18.0.1")), "benchmarking");
        assert!(is_denied_ip(ip("240.0.0.1")), "reserved");
    }

    #[test]
    fn public_addresses_are_allowed() {
        // CONVENTIONS §7.3 mode 3 (bring-your-own JWT via JWKS) must stay reachable:
        // an over-broad deny list would break every legitimate public IdP.
        assert!(!is_denied_ip(ip("1.1.1.1")));
        assert!(!is_denied_ip(ip("8.8.8.8")));
        assert!(!is_denied_ip(ip("2606:4700:4700::1111")));
        assert!(
            !is_denied_ip(ip("172.32.0.1")),
            "just outside 172.16.0.0/12"
        );
        assert!(!is_denied_ip(ip("2001:4860:4860::8888")));
    }

    #[tokio::test]
    async fn non_https_scheme_is_rejected() {
        let error = validate_jwks_url("http://idp.example.com/jwks", &hardened())
            .await
            .expect_err("http must be rejected when the dev override is off");
        assert_eq!(error.reason(), JwksDenialReason::Scheme);
    }

    #[tokio::test]
    async fn non_https_scheme_is_permitted_under_the_dev_override() {
        let url = validate_jwks_url("http://idp.example.com/jwks", &dev_override())
            .await
            .expect("the dev override must permit http so local IdPs stay usable");
        assert_eq!(url.scheme(), "http");
    }

    #[tokio::test]
    async fn literal_denied_ip_hosts_are_rejected_without_dns() {
        for raw in [
            "https://169.254.169.254/latest/meta-data/",
            "https://127.0.0.1/jwks",
            "https://[::1]/jwks",
            "https://[::ffff:169.254.169.254]/jwks",
        ] {
            let error = validate_jwks_url(raw, &hardened())
                .await
                .err()
                .unwrap_or_else(|| panic!("{raw} must be rejected"));
            assert_eq!(
                error.reason(),
                JwksDenialReason::IpRange,
                "{raw} must be denied by address range, not by another rule"
            );
        }
    }

    #[tokio::test]
    async fn dev_override_permits_a_loopback_host() {
        let url = validate_jwks_url("http://127.0.0.1:8080/jwks", &dev_override())
            .await
            .expect("the dev override must permit loopback IdPs");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
    }

    #[tokio::test]
    async fn a_non_absolute_url_is_rejected() {
        let error = validate_jwks_url("/jwks", &hardened())
            .await
            .expect_err("a relative url must be rejected");
        assert_eq!(error.reason(), JwksDenialReason::Url);
    }

    #[tokio::test]
    async fn a_well_formed_jwks_is_fetched_and_parsed() {
        let stub = spawn(StubPlan::json(EMPTY_JWKS)).await;
        let jwks = fetch_jwks_hardened(&Client::new(), &stub.url, &dev_override())
            .await
            .expect("a valid jwks must be accepted");
        assert!(jwks.keys.is_empty());
    }

    #[tokio::test]
    async fn the_registered_jwk_set_content_type_is_accepted() {
        // Asserted explicitly: rejecting `application/jwk-set+json` would break real
        // IdPs that use the registered media type.
        let stub = spawn(StubPlan {
            content_type: "application/jwk-set+json; charset=utf-8",
            ..StubPlan::json(EMPTY_JWKS)
        })
        .await;
        fetch_jwks_hardened(&Client::new(), &stub.url, &dev_override())
            .await
            .expect("application/jwk-set+json must be accepted");
    }

    #[tokio::test]
    async fn a_non_json_content_type_is_rejected_before_parsing() {
        let stub = spawn(StubPlan {
            content_type: "text/html",
            body: "<html>internal service</html>".to_string(),
            ..StubPlan::json(EMPTY_JWKS)
        })
        .await;
        let error = fetch_jwks_hardened(&Client::new(), &stub.url, &dev_override())
            .await
            .expect_err("a non-JSON content type must be rejected");
        assert_eq!(error.reason(), JwksDenialReason::ContentType);
    }

    #[tokio::test]
    async fn an_oversized_body_is_rejected_by_the_streaming_counter() {
        // Chunked, so there is no `Content-Length` to check: only the running counter
        // over `bytes_stream()` can catch this, which is the difference between a
        // real cap and a decorative one.
        let body = "x".repeat(64 * 1024);
        let stub = spawn(StubPlan {
            chunked: true,
            ..StubPlan::json(&body)
        })
        .await;
        let settings = JwksFetchSettings {
            max_response_bytes: 512,
            ..dev_override()
        };
        let error = fetch_jwks_hardened(&Client::new(), &stub.url, &settings)
            .await
            .expect_err("a body over the cap must be rejected");
        assert_eq!(error.reason(), JwksDenialReason::Size);
    }

    #[tokio::test]
    async fn a_declared_length_over_the_cap_is_rejected_early() {
        let stub = spawn(StubPlan {
            declared_length: Some(1_000_000),
            ..StubPlan::json(EMPTY_JWKS)
        })
        .await;
        let settings = JwksFetchSettings {
            max_response_bytes: 512,
            ..dev_override()
        };
        let error = fetch_jwks_hardened(&Client::new(), &stub.url, &settings)
            .await
            .expect_err("a declared length over the cap must be rejected");
        assert_eq!(error.reason(), JwksDenialReason::Size);
    }

    #[tokio::test]
    async fn a_held_response_is_abandoned_at_the_configured_timeout() {
        // The stub holds the first response until notified and the test never
        // releases it, so the deadline — not a `sleep` — ends the fetch.
        let hold = Arc::new(tokio::sync::Notify::new());
        let stub = spawn(StubPlan {
            hold: Some(hold.clone()),
            ..StubPlan::json(EMPTY_JWKS)
        })
        .await;
        let settings = JwksFetchSettings {
            timeout_ms: 150,
            ..dev_override()
        };
        let error = fetch_jwks_hardened(&Client::new(), &stub.url, &settings)
            .await
            .expect_err("a held response must be abandoned at the timeout");
        assert_eq!(error.reason(), JwksDenialReason::Timeout);
        hold.notify_waiters();
    }

    #[tokio::test]
    async fn a_non_success_status_is_rejected() {
        let stub = spawn(StubPlan {
            status_line: "HTTP/1.1 500 Internal Server Error",
            ..StubPlan::json(EMPTY_JWKS)
        })
        .await;
        let error = fetch_jwks_hardened(&Client::new(), &stub.url, &dev_override())
            .await
            .expect_err("a 5xx upstream must be rejected");
        assert_eq!(error.reason(), JwksDenialReason::Status);
    }

    #[tokio::test]
    async fn a_json_body_that_is_not_a_jwks_is_rejected() {
        let stub = spawn(StubPlan::json(r#"{"ami-id":"ami-0abcd"}"#)).await;
        let error = fetch_jwks_hardened(&Client::new(), &stub.url, &dev_override())
            .await
            .expect_err("a non-JWKS JSON document must be rejected");
        assert_eq!(error.reason(), JwksDenialReason::Parse);
    }

    #[tokio::test]
    async fn the_hardened_fetch_refuses_a_loopback_url_without_the_dev_override() {
        // The same stub the tests above talk to is unreachable under the production
        // posture — proof the hardening is what permits it, not the transport.
        let stub = spawn(StubPlan::json(EMPTY_JWKS)).await;
        let error = fetch_jwks_hardened(&Client::new(), &stub.url, &hardened())
            .await
            .expect_err("loopback must be refused when the dev override is off");
        assert_eq!(error.reason(), JwksDenialReason::Scheme);
        assert_eq!(stub.hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn denial_details_never_reach_the_caller() {
        let error = JwksFetchError::new(
            JwksDenialReason::IpRange,
            "jwks url resolves to denied address 169.254.169.254",
        );

        let unauthorized = error.into_unauthorized();
        let body = format!("{:?}", unauthorized.error_response(None));
        assert!(
            !body.contains("169.254.169.254"),
            "the resolved address must not reach the caller: {body}"
        );
        assert!(
            !body.contains("ip_range"),
            "the denial reason must not reach the caller: {body}"
        );

        let registration = error.into_registration_error();
        let body = format!("{:?}", registration.error_response(None));
        assert!(
            body.contains("jwks_url_rejected"),
            "registration rejections carry the catalogued code: {body}"
        );
        assert!(
            !body.contains("169.254.169.254"),
            "the resolved address must not reach the admin either: {body}"
        );
    }
}
