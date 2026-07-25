# Plan 03 — QA report (security hardening)

Merged as [#25](https://github.com/nurcahyo/moira/pull/25), commit `19b98ae`.

Four adversarial lenses: SSRF/JWKS, hashing migration, middleware/availability, test integrity.
Ten implementation agents, then five remediation agents.

## Verdict

The first pass **would have shipped an authentication bypass**. The QA caught it, plus two entirely
missed SSRF sinks and an unauthenticated denial of service. All were fixed and empirically re-verified
before merge. Two of the plan's own tests were failing at review time.

## The critical finding — JWKS redirects

`jwks_url` fetches followed redirects and never re-validated the target. `AppState.http` was built
without a redirect policy, so reqwest's default `Policy::limited(10)` applied, and `https_only` defaults
to `false`, permitting an https→http downgrade. Proven end to end under the exact production posture
(`allow_insecure_dev_urls = false`):

```
jwks_url = https://<open-redirector>/redirect-to?url=http%3A%2F%2F127.0.0.1%3A51769%2Finternal
fetch_jwks_hardened(...) -> Ok(keys=0)
loopback-target hit counter: 0 -> 1
```

Reproduced independently against a second redirector; a 9-hop chain was followed to completion.

One 302 defeated the scheme rule *and* the entire IP deny list simultaneously. This is worse than
ordinary SSRF because the fetched body is accepted as **trust material**: any internal endpoint
answering `application/json` with `{"keys":[...]}` becomes the token-signing key set for that issuer —
a complete authentication bypass. Plan 07 was to be built on this path.

**Fix.** A dedicated JWKS client with `redirect::Policy::none()`. `AppState.http` is deliberately
untouched, since the same client serves provider execution and changing its redirect behaviour would be
an unrelated production change. A final-URL check is layered on as a second line.

**Negative control** — the part that makes this trustworthy: restoring `Policy::limited(10)` flips both
tests red with `target_hits=1`. So `Policy::none()` is the load-bearing control; the URL check alone
would still let the internal endpoint be contacted. Post-fix verification:

```
PROD POSTURE: status=401 reasons=["ip_range"] redirector_hits=0 target_hits=0
ONE HOP:      status=401 reasons=["redirect"]  redirector_hits=1 target_hits=0
MULTI HOP:    status=401 reasons=["redirect"]  a=1 b=0 c=0
```

## Two SSRF sinks the first pass missed entirely

**Registration-time validation had no production call site.** `validate_jwks_url` existed, was unit
tested, and was re-exported — but grep found no caller. `POST /api/v1/admin/jwt-issuers` with
`jwks_url: https://169.254.169.254/latest/meta-data/` returned **201 Created** and persisted the row.
Reproduced on a freshly created database, so it was not test residue. These were the two failing tests.

**`POST .../refresh-jwks` was completely unhardened** — a bare `http.get(url).send()` with no validation,
no timeout, no size cap, no content-type check, following redirects, and not even populating the cache.
It also mapped the upstream status into the admin's response, making it a clean reachability and
port-scan oracle over the pod's internal network, driven by an ordinary admin credential. Now hardened,
and it returns one collapsed error regardless of what the upstream did.

## Unauthenticated denial of service

The SSE group carried no timeout at all. `TimeoutLayer` bounds the response-*head* future, which for a
body-consuming handler **includes body ingestion** — so exempting the group removed the request-read
bound too, not just the response-body bound the code comment reasoned about.

```
/api/v1/responses          -> 504 Gateway Timeout
/api/v1/responses/stream   -> NO RESPONSE after 4s (held open)
/v1/responses              -> NO RESPONSE after 4s (held open)
ADV-PARKED: 200/200 unauthenticated header-only connections still parked
```

A client sending only headers with a `Content-Length` pinned a connection task and an FD forever, and
`public_actor()` runs *after* `Json` extraction, so no credential was needed. Fixed with
`RequestBodyTimeoutLayer`; verified 20/20 released, slowest 30.02s — while a real SSE stream still
outlives the head timeout and delivers its terminal event. Both directions were checked: a fix that
kills streaming would be an outage.

## Probe routes shipped in release binaries

`/internal/test/panic` and `/internal/test/slow` sat behind a Cargo feature, and `Cargo.toml` claimed
`cargo build --release` therefore could not contain them. True only of the *default* feature set —
and this repo's own CI uses `--all-features`. Confirmed against the actual binary: 1 hit each. The gate
is now `debug_assertions`, not the feature, so no flag combination can ship them; the build refuses with
`compile_error!`. Shipped binary now greps 0.

## IPv6 encodings of denied addresses

`to_ipv4_mapped()` matched only the two forms with 80 zero bits, so these all passed validation:
RFC 6052 NAT64 (`64:ff9b::a9fe:a9fe`), RFC 3056 6to4 (`2002:a9fe:a9fe::1`), IPv4-translated
`::ffff:0:0/96`, `fec0::/10`, `0.0.0.0/8` beyond the exact zero address, and `192.88.99.0/24`. On an
IPv6-only NAT64+DNS64 cluster — standard for IPv6-only GKE and EKS node pools — that reaches the IMDS.
Now classified by *embedded* address rather than by prefix, so NAT64-only clusters still reach
legitimate public IdPs.

Worth recording what the reviewer tried and could **not** break, so it is not re-litigated: decimal
(`https://2130706433/`), hex, octal, trailing-dot hostnames, userinfo, `https://0/`, every `::ffff:`
mapped form, `::`, `::1`, `fd00::/8`, `fe80::/10`, non-default ports, and DNS names resolving into denied
space. The multi-address DNS rule correctly refuses if *any* resolved address is denied.

## Four unfalsifiable security claims

| Mutation | Before | After |
|---|---|---|
| `if false && is_denied_ip(ip)` | survived on the shared DB | **killed** — 3 tests fail |
| singleflight lock removed | survived 10/10 e2e | **killed** at unit level |
| `None => false` (dual-read disabled) | survived 7/7 | **killed** at both layers |
| drop `.layer(timeout)` from admin+conversation groups | survived the whole suite | **killed** |

The first is the instructive one. It survived because two tests used hardcoded, non-fixture-suffixed
URLs while audit assertions matched rows from *previous runs*. **347 stale issuer rows and 165 audit
rows** had accumulated in the shared test database, silently satisfying the assertions. A test suite can
rot into permanent green without anyone touching it. Purged, and the tests are now scoped by both a
suffixed URL and `occurred_at`.

## What survived attack

The streaming size cap is genuinely streaming, not `Content-Length`-trusting — an infinite chunked body
with no `Content-Length` was rejected after the cap with memory bounded, and a lying `Content-Length` was
truncated by hyper rather than buffered. The timeout fires. Stale-cache retention keeps auth working
without failing open. No oracle leakage on the verification path. Legitimate public IdPs all still
work under the production posture: Google, Microsoft, GitHub Actions, Apple, GitLab, Facebook, Yahoo.

The hashing mechanism also survived: the pepper is genuinely keyed in (constant-key mutation killed a
test), genuinely wired through `AppState` (sourcing the wrong pepper failed 4 of 7 e2e tests), output
fits `varchar(128)` so the no-migration claim holds, every producer call site was swapped, and dual
lookup exists on all three ledger users.

## Gates

All five green, raw, captured to file and read back: fmt clean; clippy `--workspace --all-targets
--all-features -D warnings` clean; **194 lib + 84 integration, 0 failed**; release build clean; and the
full suite green from an empty database. Per-suite wall clock recorded; the only `0.00s` entries have
zero tests.

## Tooling failure worth recording

`rtk`, **including `rtk proxy`**, was independently caught fabricating output by three reviewers:
reporting failures that did not occur, inventing test names, and printing a source line that does not
exist in the file. One agent saw it report "173 passed; 1 failed" where the raw run was 194/0. Every
number in this report was captured with `/bin/sh -c '... > file'` and read back. A fabricated green on a
security gate is precisely the failure mode that ships a bypass.

## Deferred

Seven items in `TODO.md`. The two that matter most: **`actor_fingerprint` is still unkeyed** — the one
column in `idempotency_records` left on plain SHA-256, and plan 07 is what fills it with human identity;
and **stale-JWKS retention is unbounded in time**, so a key revoked at the IdP stays trusted for as long
as the endpoint keeps failing.
