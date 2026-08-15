//! The address rules a **provider endpoint** must satisfy — the one place they are written.
//!
//! # Why this module exists at all
//!
//! Issue #251's finding 2 was one instance of a class: *a decrypted secret leaves the process
//! to an address taken from data nothing validated*. Round one closed the reported instance
//! (`providers.metadata["oauth_token_endpoint"]`, see
//! `crate::infra::workers::oauth_refresh`). It did not close the class, and the class has
//! more members, because a provider's *runtime* endpoint has never been settled in one place:
//!
//! | Address a credential is sent to | Where it comes from | Validated before this module |
//! |---|---|---|
//! | `providers.base_url` | admin write path | yes — [`validate_provider_base_url`](crate::application::admin::validate_provider_base_url) at write time only |
//! | `provider_credentials.<payload>.endpoint` (Azure) | the **encrypted credential payload** | **no** — `validate_credential_secret` matched `AzureOpenAi { api_key, .. }` and never looked at `endpoint` |
//! | `providers.metadata["oauth_token_endpoint"]` | free-form JSON | yes, since round one |
//!
//! The middle row is the open member. `RuntimeFactory::build_completion_model` and
//! `EmbeddingFactory::build_embedding_model` both read `credential.config["endpoint"]` **in
//! preference to** `provider.base_url` for `azure_openai`, and then hand the decrypted API
//! key to a client pointed at it. So `moira:credentials:write` alone — a scope that never
//! goes near the providers surface — could aim a credential-bearing request at
//! `http://169.254.169.254/`, `http://127.0.0.1:6379/`, or any other address this
//! deployment's `provider_security` policy forbids `base_url` from naming. The credential
//! surface only ever returns *masked* secrets, so this was also the one write on that surface
//! whose destination nothing reviewed.
//!
//! # Shape, not resolution
//!
//! [`provider_endpoint_shape_denial`] is pure and performs **no DNS**. That is deliberate and
//! it is a real limitation, stated rather than hidden:
//!
//! * It is the check the *use-time* callers can afford. `build_completion_model` runs on
//!   every execution attempt; a resolution there would add a blocking dependency (and a new
//!   outage mode) to the hot path, and would resolve a name `reqwest` is about to resolve
//!   again anyway.
//! * The DNS half — a public name that *resolves* into a denied range — is caught on the
//!   write path, where it costs nothing per request: `validate_provider_base_url` keeps its
//!   `resolves_to_forbidden_ip` step, and this change adds the same write-time treatment to
//!   the Azure credential `endpoint`.
//!
//! So the honest summary is: **write time is the complete check, use time is the check
//! nothing can get behind.** A row written straight into the database, or carried in by a
//! migration, still cannot send a secret to an IP literal or a `localhost` name in a denied
//! range — but a hostile *public name that resolves privately* is only refused if it went
//! through an admin write.
//!
//! # Why this is not `security::ssrf::validate_outbound_url`
//!
//! That guard's `allow_insecure` is a single all-or-nothing bypass, and `provider_security`
//! spells its concessions out as two independent flags (`allow_http_provider_urls` for the
//! scheme, `allow_private_provider_urls` for the address space). Routing the provider surface
//! through the all-or-nothing flag would make use time **stricter** than write time for the
//! one deployment shape that matters: an operator who runs an in-cluster provider on a
//! private `https` address sets `allow_private_provider_urls` alone, write time accepts it,
//! and an `allow_insecure = private && http` conjunction at use time would then refuse every
//! execution against a provider the admin plane had already blessed. A use-time guard that
//! rejects what the write path accepted is an outage, not a control, so this module mirrors
//! `validate_provider_base_url`'s two-flag semantics exactly — and shares its implementation,
//! below, so the two cannot drift.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Why a provider endpoint was refused. Carries no free text a caller could be shown: the
/// mapping to a public message belongs to whichever layer is answering, because the same
/// denial is a `400` on the admin write path and a configuration failure at execution time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderEndpointDenial {
    /// Not a parseable absolute URL.
    Unparseable(String),
    /// Scheme was not `https`, and `allow_http` was not granted.
    Scheme,
    /// The URL embeds a username or password — those are sent to whoever the host turns out
    /// to be, on the same request that carries the credential.
    Credentials,
    /// No host component at all.
    NoHost,
    /// A cloud instance-metadata address. Refused regardless of `allow_private`: there is no
    /// development story in which Moira should post a provider credential to
    /// `169.254.169.254`.
    CloudMetadata,
    /// A loopback, private, link-local or otherwise non-routable host, and `allow_private`
    /// was not granted.
    PrivateAddress,
}

impl ProviderEndpointDenial {
    /// Stable, machine-filterable token for log fields — the same posture
    /// [`crate::security::OutboundDenialReason::as_str`] takes.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Unparseable(_) => "unparseable",
            Self::Scheme => "scheme",
            Self::Credentials => "credentials",
            Self::NoHost => "no_host",
            Self::CloudMetadata => "cloud_metadata",
            Self::PrivateAddress => "private_address",
        }
    }
}

/// Whether this deployment waives part of the provider address policy.
///
/// The two flags are independent on purpose and are read from
/// [`crate::config::ProviderSecuritySettings`] — see the module header for why collapsing
/// them into one "insecure" bit would make use time stricter than write time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderEndpointPolicy {
    /// `provider_security.allow_private_provider_urls`.
    pub allow_private: bool,
    /// `provider_security.allow_http_provider_urls`. Rejected outright by
    /// `Settings::validate_production`, so this is never set in production.
    pub allow_http: bool,
}

impl ProviderEndpointPolicy {
    pub fn from_provider_security(security: &crate::config::ProviderSecuritySettings) -> Self {
        Self {
            allow_private: security.allow_private_provider_urls,
            allow_http: security.allow_http_provider_urls,
        }
    }

    /// The use-time gate. `Ok(())` means the address is one this deployment has already
    /// declared a provider may be reached at.
    pub fn permits(&self, value: &str) -> Result<(), ProviderEndpointDenial> {
        match provider_endpoint_shape_denial(value, self.allow_private, self.allow_http) {
            Some(denial) => Err(denial),
            None => Ok(()),
        }
    }
}

/// Every provider-endpoint rule that needs no I/O, in one place.
///
/// `None` means "nothing refusable without resolving the host". The write path adds the DNS
/// step on top (`validate_provider_base_url`); use-time callers stop here.
///
/// Order matters and is part of the contract, for the same reason
/// [`crate::security::validate_outbound_url`] documents it: everything refusable for free is
/// refused before anything that costs a lookup, so the guard can never be used as a way to
/// make Moira perform arbitrary resolutions.
pub fn provider_endpoint_shape_denial(
    value: &str,
    allow_private: bool,
    allow_http: bool,
) -> Option<ProviderEndpointDenial> {
    let trimmed = value.trim().trim_end_matches('/');
    let parsed = match url::Url::parse(trimmed) {
        Ok(parsed) => parsed,
        Err(err) => return Some(ProviderEndpointDenial::Unparseable(err.to_string())),
    };
    match parsed.scheme() {
        "https" => {}
        "http" if allow_http => {}
        _ => return Some(ProviderEndpointDenial::Scheme),
    }
    if parsed.username() != "" || parsed.password().is_some() {
        return Some(ProviderEndpointDenial::Credentials);
    }
    let Some(host) = parsed.host_str() else {
        return Some(ProviderEndpointDenial::NoHost);
    };
    if is_cloud_metadata_host(host) {
        return Some(ProviderEndpointDenial::CloudMetadata);
    }
    if !allow_private && is_private_host(host) {
        return Some(ProviderEndpointDenial::PrivateAddress);
    }
    None
}

pub fn is_cloud_metadata_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "169.254.169.254"
            | "metadata.google.internal"
            | "metadata"
            | "instance-data"
            | "100.100.100.200"
    )
}

/// # The brackets are stripped, and that is a fix, not a formality
///
/// This body moved here verbatim from `application::admin::shared` except for the
/// `strip_prefix('[')` pair, and that pair closes a hole the original had: `Url::host_str`
/// returns an IPv6 host **with** its brackets (`"[::1]"`, `"[fe80::1]"`), and
/// `"[::1]".parse::<IpAddr>()` is an `Err`. So every IPv6 literal — loopback, link-local,
/// unique-local — read as "not an IP address at all" and fell through the address rule.
/// `https://[::1]:8443/` was an accepted `providers.base_url` before this change, on a
/// deployment with `allow_private_provider_urls` off.
///
/// It is stated here rather than left to be rediscovered: this is the one place where the
/// shared rules are *stricter* than the ones `validate_provider_base_url` enforced before, and
/// an existing row carrying an IPv6-literal base URL will now be refused at use time (and on
/// its next admin write) unless the deployment grants `allow_private_provider_urls`. That is
/// the intended outcome — the alternative is a credential-bearing request to `[::1]`.
pub fn is_private_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") {
        return true;
    }
    let literal = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    literal.parse::<IpAddr>().is_ok_and(is_forbidden_ip)
}

pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_multicast()
                || ip == Ipv4Addr::new(0, 0, 0, 0)
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || is_ipv6_unicast_link_local(ip)
                || ip.is_multicast()
        }
    }
}

fn is_ipv6_unicast_link_local(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

#[cfg(test)]
mod tests {
    use super::*;

    fn denial(value: &str) -> Option<ProviderEndpointDenial> {
        provider_endpoint_shape_denial(value, false, false)
    }

    #[test]
    fn an_ordinary_public_https_endpoint_is_permitted() {
        assert_eq!(denial("https://example.openai.azure.com/"), None);
    }

    #[test]
    fn the_cloud_metadata_address_is_refused_even_with_every_flag_granted() {
        assert_eq!(
            provider_endpoint_shape_denial("http://169.254.169.254/latest/meta-data/", true, true),
            Some(ProviderEndpointDenial::CloudMetadata),
            "no development story justifies posting a provider credential to the instance \
             metadata service"
        );
        assert_eq!(
            provider_endpoint_shape_denial("https://metadata.google.internal/", true, true),
            Some(ProviderEndpointDenial::CloudMetadata)
        );
    }

    #[test]
    fn loopback_and_rfc1918_literals_are_refused_by_default_and_allowed_by_the_private_flag() {
        assert_eq!(
            denial("https://127.0.0.1:8443/v1"),
            Some(ProviderEndpointDenial::PrivateAddress)
        );
        assert_eq!(
            denial("https://10.1.2.3/v1"),
            Some(ProviderEndpointDenial::PrivateAddress)
        );
        assert_eq!(
            denial("https://localhost/v1"),
            Some(ProviderEndpointDenial::PrivateAddress)
        );
        assert_eq!(
            provider_endpoint_shape_denial("https://10.1.2.3/v1", true, false),
            None,
            "an in-cluster https provider is exactly what allow_private_provider_urls buys"
        );
    }

    /// The two flags are independent: `allow_private` alone must not also waive the scheme
    /// rule, and `allow_http` alone must not also waive the address rule. This is the
    /// property that keeps use time from being stricter than write time — see the module
    /// header.
    #[test]
    fn the_scheme_and_address_concessions_are_independent() {
        assert_eq!(
            provider_endpoint_shape_denial("http://public.example/v1", true, false),
            Some(ProviderEndpointDenial::Scheme)
        );
        assert_eq!(
            provider_endpoint_shape_denial("https://192.168.0.9/v1", false, true),
            Some(ProviderEndpointDenial::PrivateAddress)
        );
        assert_eq!(
            provider_endpoint_shape_denial("http://192.168.0.9/v1", true, true),
            None
        );
    }

    #[test]
    fn an_endpoint_that_embeds_credentials_is_refused() {
        assert_eq!(
            denial("https://user:pass@api.example/v1"),
            Some(ProviderEndpointDenial::Credentials)
        );
    }

    #[test]
    fn a_non_absolute_or_schemeless_value_is_refused() {
        assert!(matches!(
            denial("api.example/v1"),
            Some(ProviderEndpointDenial::Unparseable(_))
        ));
        assert_eq!(
            denial("file:///etc/passwd"),
            Some(ProviderEndpointDenial::Scheme)
        );
    }

    /// The `url` crate normalises the decimal and hex IPv4 forms into `Host::Ipv4`, so those
    /// bypasses land on the literal check rather than slipping through as domain names.
    #[test]
    fn the_obfuscated_ipv4_forms_are_classified_as_literals() {
        assert_eq!(
            denial("https://2130706433/v1"),
            Some(ProviderEndpointDenial::PrivateAddress)
        );
        assert_eq!(
            denial("https://0x7f.0.0.1/v1"),
            Some(ProviderEndpointDenial::PrivateAddress)
        );
    }

    /// The bracketed form is what `Url::host_str` actually hands back, so
    /// [`is_private_host`] is pinned against it directly and not only through a full URL —
    /// the bug was in this one predicate, and this is the assertion that reds if the
    /// bracket-stripping is removed.
    #[test]
    fn is_private_host_recognises_a_bracketed_ipv6_literal() {
        assert!(is_private_host("[::1]"));
        assert!(is_private_host("[fe80::1]"));
        assert!(is_private_host("[fc00::1]"));
        assert!(!is_private_host("[2606:4700::1111]"));
        assert!(is_private_host("::1"), "the unbracketed form still works");
    }

    /// Regression for the bracket bug described on [`is_private_host`]: before this module,
    /// every one of these was an accepted `providers.base_url`.
    #[test]
    fn ipv6_loopback_and_link_local_are_refused() {
        assert_eq!(
            denial("https://[::1]:8443/v1"),
            Some(ProviderEndpointDenial::PrivateAddress)
        );
        assert_eq!(
            denial("https://[fe80::1]/v1"),
            Some(ProviderEndpointDenial::PrivateAddress)
        );
    }
}
