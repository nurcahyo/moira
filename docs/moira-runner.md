# `moira-runner` — the containerised Claude runner control service

Workstream R1 of [#272](https://github.com/nurcahyo/moira/issues/272), tracked in
[#273](https://github.com/nurcahyo/moira/issues/273).

`moira-runner` is the **second binary of this crate** and the only component permitted to talk
to the Docker Engine API. It provisions containers that run `claude setup-token`, exposes the
authorization URL scraped from the container's tty, accepts the operator's pasted
authorization code, and yields the minted token exactly once.

```
Console (no Docker) -> Moira (no Docker) -> moira-runner -> Docker Engine API
```

The **image** this service starts is workstream R4's, built by
`scripts/build-claude-runner-image.sh` from `deploy/claude-runner/Dockerfile`; that script
prints the content-pinned `sha256:` id to put in `MOIRA_RUNNER__IMAGE`. See
[`claude-runners.md`](claude-runners.md) for the image, the operator walkthrough and the
honest "N containers on one account is not N× capacity" limit. This document covers the
service.

## Why a separate process

Docker socket access is **root-equivalent on the host**: anything that can create a container
can bind-mount `/` and read or write whatever the daemon's user can. Neither the console nor
the Moira API process may hold that capability. `moira-runner` holds it, binds loopback by
default, authenticates every request with a control token, and is never exposed to the
internet.

Inside the binary the capability is confined further: `bollard` is imported by exactly one
file (`src/runner/docker_engine.rs`), which a unit test in `src/runner/mod.rs` asserts, and
everything else goes through the `ContainerEngine` trait. That trait has nine operations and
no way to express a bind mount, a published port, or `privileged` — so widening what a runner
container may do requires widening the trait first, in a diff a reviewer can see.

## The measured facts this rests on

All measured 2026-08-15 against Docker Desktop 29.6.2 and `@anthropic-ai/claude-code` 2.1.233.
Evidence: <https://github.com/nurcahyo/moira/issues/272#issuecomment-5303491808>.

| Fact | Consequence for the design |
|---|---|
| Inside a container, `claude setup-token` does **not** use a loopback callback — it uses Anthropic's hosted redirect (`platform.claude.com/oauth/code/callback`) and a paste prompt. | Nothing is published, forwarded or port-mapped. `ContainerSpec` has no port field. |
| A container created with `Tty: true` gets its pty **from the daemon**; a plain pipe yields **zero bytes**. | `Tty`, `OpenStdin` and `StdinOnce: false` are fixed, not configurable. |
| `docker exec -i -t` refuses piped stdin; the Engine API **attach** endpoint on a `-dit` container does not (HTTP 101, and the write reaches the prompt). | `write_stdin` uses `POST /containers/{id}/attach`. This is what `node-pty` was wanted for. |
| The Ink UI **hard-wraps the authorization URL across lines** at terminal width. | `src/runner/scrape.rs` strips ANSI and rejoins wrapped lines before matching. |

A fifth fact was found while building this, by capturing the stream byte for byte, and it
corrects the fourth: the CLI emits the **complete** URL inside an **OSC 8 hyperlink**, with the
wrapped fragments as its visible text. The first implementation stripped the hyperlink away and
reassembled the fragments — and got the reassembly wrong, returning a URL truncated 80
characters in with `state` and `code_challenge` missing. It looked like a URL. `scrape.rs` now
prefers the hyperlink target and keeps the rejoin as a tested fallback; both paths are pinned
by a unit test built from the real capture.

## What was verified against a real runner, and what was not

Verified on 2026-08-16 by running the built `moira-runner` binary against
`moira-claude-runner:local` (`claude setup-token` 2.1.233) on Docker Desktop 29.6.2:

- `GET /healthz` reports `docker: reachable`.
- `POST /v1/runners` creates and starts a hardened container.
- `GET /v1/runners/{id}` reaches `awaiting_authorization` and returns the **complete**
  authorization URL, including `code_challenge` and `state`.
- `POST /v1/runners/{id}/authorization-code` submits the line and the CLI performs the
  exchange — `OAuth error: … status code 400` for a deliberately bogus code, reproduced twice,
  after which the runner reports `failed` / `oauth_exchange_failed`. See below: the delivery
  sequence that achieves this is specific and measured.
- The reaper removes an expired runner and `GET` then answers `404`.

`bollard` against a real daemon is additionally covered by `tests/runner_docker_engine.rs`
(container lifecycle, label discovery, rename, the attach write, and that a runner cannot see
the Docker socket) — all three tests pass against a real daemon.

### Code submission: the sequence is measured, not reasoned about

Code submission **works** end to end against the real image, and it works because of a specific
and unintuitive sequence. Every plausible simplification has been tried and measured not to
submit, so treat this as load-bearing.

`claude setup-token` does **not** submit on the carriage return glued to the end of the pasted
text. The code appears masked at the prompt and the line just sits there — no error, no
timeout, the runner stays in `awaiting_authorization` until its TTL. It submits on a **second,
bare** carriage return delivered on the same still-open connection a few seconds later. So the
shipped sequence is: attach and start draining; wait 1.5 s; write `code + "\r"` unchanged; wait
5 s; write one bare `\r`; keep the connection open and draining for 30 s.

Verified twice, independently, against `moira-claude-runner:local` (`claude setup-token`
2.1.233, Docker Desktop 29.6.2): both runs produced `OAuth error: Request failed with status
code 400` for a deliberately bogus code, and the runner's own state machine then reported
`failed` / `oauth_exchange_failed`.

What does **not** work, so nobody tidies the code into one of them:

| Sequence | Result |
|---|---|
| `code + "\r"` in one write, connection closed straight after | text lands, no submit |
| `code + "\r"` in one write, connection held open 30 s while draining | text lands, no submit |
| `code` alone, then a bare `\r` after 2 s or after 5 s | text lands, no submit |
| `code` and `\r` as two writes 100 ms / 250 ms apart | text lands, no submit |
| bracketed paste `ESC[200~…ESC[201~` | worse — the terminator is typed literally, so this CLI does not implement it |

Row three kills the obvious theory: "deliver the carriage return as its own read" is not
sufficient on its own — the payload's own trailing `\r` has to be there too. The mechanism
inside the CLI was not chased further.

Two theories were eliminated on the way, both worth recording:

- **`bollard` was never at fault.** A hand-rolled HTTP 101 upgrade over the raw unix socket
  behaved identically. It was reverted rather than shipped, since it added ~150 lines of
  bespoke HTTP and two tokio features for no measured benefit.
- **The write half was never half-closed.** A chunk count cannot test that — Docker delivers
  only *new* output on attach, so a quiet CLI yields zero chunks whether or not the writer is
  alive. What settled it: a diagnostic wrote `code + "\r"`, waited 5 s, then wrote a bare `\r`
  on the *same* writer. The probe write returned `Ok`, proving the write half had been alive
  throughout — and the container immediately performed the exchange. One run eliminated the
  theory and revealed the working sequence at once.

Three bugs on this side were found by the same investigation and are fixed:

1. `awaiting_authorization` used to mean only "a URL has been scraped", which let a caller
   submit before the CLI's reader existed. It now also requires the paste prompt.
2. Containers were created with `AttachStdin: true`, which tells the daemon to expect a client
   attached at start time. They are now created detached, exactly as `docker run -dit`.
3. The attach was torn down as soon as the write resolved. It now outlives the call.

**Still unproven:** capturing a *minted* token on a **valid** code. Every runner driven here
used a deliberately bogus code, so the success path — the token's exact format — has never been
seen. `MOIRA_RUNNER__TOKEN_PREFIX` is configurable (default `sk-ant-`) so it can be corrected
without a rebuild, and the tests assert the scraping *mechanism* rather than the format.

## Configuration

Every variable is `MOIRA_RUNNER__*`. The runner does **not** read `Settings` and needs no
database, no Redis, and no master key.

| Variable | Default | Notes |
|---|---|---|
| `MOIRA_RUNNER__CONTROL_TOKEN` | *(required)* | Bearer token for every route except `/healthz`. ≥32 bytes, ≥8 distinct bytes, no placeholder words. `openssl rand -hex 32`. |
| `MOIRA_RUNNER__IMAGE` | *(required)* | Content-pinned: `registry/name@sha256:<64 hex>` **or** a local image id `sha256:<64 hex>`. A mutable tag is refused. |
| `MOIRA_RUNNER__BIND` | `127.0.0.1:8090` | A non-loopback address without `ALLOW_INSECURE_BIND` is a startup error. |
| `MOIRA_RUNNER__DOCKER_HOST` | *(unset — falls back to `DOCKER_HOST`, then the platform default)* | On Docker Desktop for macOS set it to `unix://$HOME/.docker/run/docker.sock`; see the finding below. |
| `MOIRA_RUNNER__ALLOW_INSECURE_BIND` | `false` | Set only when TLS is terminated in front. |
| `MOIRA_RUNNER__COMMAND` | `claude setup-token` | JSON array or whitespace-separated argv. Never passed to a shell. |
| `MOIRA_RUNNER__MEMORY_BYTES` | `1073741824` | `HostConfig.Memory`. |
| `MOIRA_RUNNER__NANO_CPUS` | `1000000000` | One CPU. |
| `MOIRA_RUNNER__PIDS_LIMIT` | `256` | |
| `MOIRA_RUNNER__DEFAULT_TTL_SECONDS` | `900` | Applied when the caller asks for no TTL. |
| `MOIRA_RUNNER__MAX_TTL_SECONDS` | `3600` | Ceiling on a caller-supplied TTL. |
| `MOIRA_RUNNER__REAP_INTERVAL_SECONDS` | `30` | |
| `MOIRA_RUNNER__AUTHORIZATION_URL_PREFIX` | `https://claude.com/cai/oauth/authorize` | |
| `MOIRA_RUNNER__PASTE_PROMPT_MARKER` | `Paste code here` | A runner reaches `awaiting_authorization` only once this appears. Matched with whitespace removed and case folded, because Ink lays the prompt out with cursor-forward sequences instead of spaces. |
| `MOIRA_RUNNER__TOKEN_PREFIX` | `sk-ant-` | See "What was verified against a real runner, and what was not". |
| `MOIRA_RUNNER__FAILURE_MARKERS` | `OAuth error` | Newline-separated. |
| `MOIRA_RUNNER__LOG` | `info,moira=info` | Deliberately not `RUST_LOG`, which would reconfigure the API process too. |
| `MOIRA_RUNNER__LOG_JSON` | `false` | |

### Why the image rule accepts a bare `sha256:` id

A locally built image has **no registry digest** — a digest is assigned by a registry on push
— but it does have an image id, which is the sha256 of its config blob and is every bit as
content-addressed. Requiring a registry digest would force an operator to push a
credential-bearing image to a registry purely to satisfy a validator, which is worse for
security. What is refused in both forms is the same thing: a reference whose meaning can
change after the operator approved it.

## Running it

```bash
MOIRA_RUNNER__CONTROL_TOKEN="$(openssl rand -hex 32)" \
MOIRA_RUNNER__IMAGE="sha256:<64 hex>" \
  cargo run --bin moira-runner
```

## The control contract

Frozen; Moira's side is implemented against the same document. All responses carry
`cache-control: no-store`.

| Route | Auth | Result |
|---|---|---|
| `POST /v1/runners` | bearer | `201 {id, state, expires_at}` |
| `GET /v1/runners/{id}` | bearer | `200 {id, state, authorization_url, expires_at, error_code}` |
| `POST /v1/runners/{id}/authorization-code` | bearer | `202 {state: "exchanging"}` |
| `GET /v1/runners/{id}/token` | bearer | `200 {token}` — **one-shot**, then `410` |
| `DELETE /v1/runners/{id}` | bearer | `204`, idempotent |
| `GET /healthz` | none | `200 {status, docker}` |

Errors are `{"error": {"code", "message"}}` with `unauthorized` (401), `runner_not_found`
(404), `runner_wrong_state` (409), `token_already_retrieved` (410), `invalid_request` (400),
`docker_unavailable` (503), `runner_failed` (500).

### State machine

```
provisioning -> awaiting_authorization -> exchanging -> ready
                     |                        |
                     +------------------------+--> failed
any state (past ttl) --> expired
```

`awaiting_authorization` means **the URL and the paste prompt are both on screen**, not just
the URL. That state is what licenses a write into a container's stdin, so it has to mean a
reader is attached. A runner that has printed its URL but not yet its prompt stays
`provisioning` — while still reporting `authorization_url`, so a console can render it early —
and `POST .../authorization-code` answers `409` for that window.

## Statelessness, and the one gap in it

Docker is the registry. A restart re-reads everything from container labels, so it orphans
nothing and the reaper collects containers this process never created.

| Fact | Where it lives | Survives a restart |
|---|---|---|
| this container is a runner | label `moira.runner=1` | yes |
| runner id | label `moira.runner.id` | yes |
| expiry | label `moira.runner.expires_at` | yes |
| the token has been handed out | container **name** suffix `-consumed` | yes |
| the authorization URL, the prompt, the token, the outcome | the container's tty stream | yes |
| a code has just been submitted (`exchanging`) | in-process only | **no** |

The consumed marker is a **rename** because the Engine API cannot change a running
container's labels, and the one-shot promise is worth a durable marker: an in-memory set
would let a restart hand the same credential out twice.

`exchanging` is the one honest gap. It has no durable representation, so in the seconds
between a code being submitted and its outcome landing in the tty stream, a restarted service
reads that runner back as `awaiting_authorization`. The exchange itself is running inside the
container and is unaffected, and the outcome (`ready` / `failed`) is fully derived from
Docker, so the next poll shows the truth.

## Security posture

- The minted token is a `MintedToken` newtype whose `Debug` prints `<redacted>` and which
  zeroizes on drop. It appears in exactly one place: the body of `GET .../token`. Nothing
  logs it, including at `trace`.
- The raw tty stream is a `TtyTranscript` newtype whose `Debug` prints only a byte count. It
  is never logged wholesale.
- The control token is compared with `subtle::ConstantTimeEq`. Every authentication failure
  gets the same answer regardless of cause.
- Daemon error text is logged, never returned: it names socket paths, container ids and mount
  tables that describe the host this service exists to protect.
- An authorization code containing any control character is refused **before** it reaches the
  container. The write goes to a terminal read and the service appends the submitting `\r`, so
  an embedded `\r` would queue extra lines as input to an interactive session holding the
  operator's credentials.
- Containers are created with `CapDrop: ["ALL"]`, `SecurityOpt: ["no-new-privileges"]`,
  `Privileged: false`, memory/CPU/PID limits, and **no binds and no mounts** — the spec type
  cannot express one.
- `AutoRemove` is deliberately **false**. The token becomes readable from the log stream at
  the moment the CLI exits, and a self-removing container would delete the credential at that
  instant. The reaper bounds the lifetime instead.

### Residual exposure, stated plainly

A runner that has reached `ready` holds the minted token in its container log until it is
reaped. Taking the token renames the container but does not remove it, because the contract
requires a subsequent fetch to answer `410` rather than `404`. The window is therefore bounded
by the TTL, and the TTL should be set to the shortest value an operator can actually complete
a browser login within — the default 900s, not the 3600s ceiling.

## Testing

| Suite | Needs | What it proves |
|---|---|---|
| `src/runner/**` unit tests | nothing | config validation, redaction, ANSI/wrap scraping, state derivation, the service including one-shot and reaping, the create-body hardening, error mapping |
| `tests/runner_control_plane.rs` | nothing | the HTTP surface over a real socket against the in-memory engine: auth, every transition including illegal ones, one-shot under contention, TTL expiry, the error envelope, restart rediscovery |
| `tests/runner_docker_engine.rs` | **opt-in**, a real daemon | `bollard` against a real Docker: lifecycle, label discovery, rename, that a daemon-allocated tty renders, that the attach endpoint accepts a piped write, and that a runner cannot see the Docker socket |

```bash
# The default run is green with Docker stopped.
cargo test

# The opt-in half. Never runs in CI: there is no image there and pulling one on every PR
# would be prohibitive.
MOIRA_RUNNER_DOCKER_TESTS=1 MOIRA_RUNNER_TEST_IMAGE=alpine:3.21 \
  cargo test --test runner_docker_engine -- --nocapture
```

`MOIRA_RUNNER_TEST_IMAGE` must already be present locally; the suite never pulls, so it cannot
turn into a silent network dependency.
