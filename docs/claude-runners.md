# Claude runner containers

Issue [#272](https://github.com/nurcahyo/moira/issues/272): containerised, provisioned
Claude CLI instances. Each one mints a Claude Code subscription token
(`claude setup-token`, run inside a locked-down container) and hands it to Moira,
which stores it encrypted as an `oauth2` credential through the existing credential
chain. This is the mechanism that dissolves the blocker recorded in
[#269](https://github.com/nurcahyo/moira/issues/269) — the CLI needs a real tty to
run its interactive login, a process whose own stdin/stdout are sockets (the
console, or Moira itself) cannot give it one, but a Docker container can, and the
Engine API can attach to that container's tty over HTTP without either of those
processes needing a tty of their own.

This document is the operator's guide: what the component is, the trust boundary
it exists to preserve, how to build and pin the container image, how the
provisioning walkthrough runs end to end, and — read this part before turning any
of it on — the honest capacity limits.

Related reading: [`claude-subscription-sidecar.md`](claude-subscription-sidecar.md)
covers the *other* sanctioned Claude-subscription shape (a local OpenAI-compatible
sidecar proxy) and the policy-volatility risk that applies to both;
`plans/12-feature-expansion-brainstorm.md` §1 is where the two were compared and
decision 1 was recorded.

## What this is, and what it is not

`moira-runner` is a small, separate service that talks to the Docker Engine API on
behalf of Moira. On request it starts a container from a pinned image
(`deploy/claude-runner/Dockerfile`, built by
`scripts/build-claude-runner-image.sh`), runs `claude setup-token` inside it with a
daemon-allocated tty, surfaces the authorization URL that command prints, accepts
the authorization code back from the operator, and scrapes the minted token off
the same tty stream once the CLI has exchanged it. Moira fetches that token
through `moira-runner`'s API and stores it exactly the way any other credential is
stored — encrypted, through the existing credential-create path.

It is **not** a running proxy that answers inference requests. What consumes an
`oauth2` Claude credential once it exists — a sidecar (see the related doc above)
or a native runner that shells out to the CLI per request — is a separate,
not-yet-built decision. This document only covers minting and storing the
credential.

## Trust boundary

Docker socket access is root-equivalent on the host: anything that can talk to
the Engine API can start a container with arbitrary binds, arbitrary capabilities,
and a path back to the host filesystem. Because of that, exactly one component in
this system is allowed to hold it:

```
Console (no Docker) --> Moira API (no Docker) --> moira-runner --> Docker Engine API
```

- The console never talks to `moira-runner` directly and never sees a Docker
  socket, a container id, or a minted token. It calls Moira's own admin API
  (`/api/v1/admin/runners*`), which is authenticated and audited the same way
  every other admin route is.
- Moira's own API process never touches the Docker socket either. It calls
  `moira-runner` server-to-server, over its own bearer-token-authenticated HTTP
  API, and that is the *only* place a runner's minted token is read from before
  it goes into Moira's credential store. The token never crosses back out to the
  console.
- `moira-runner` binds `127.0.0.1` by default. Configuring it to bind a
  non-loopback address without TLS is a startup validation error unless an
  explicit opt-out is set — the same posture as every other insecure-by-default
  knob in this codebase (`docs/local-testing.md`'s "unsafe development
  configuration" warning is the same idea applied here).
- Every container `moira-runner` creates is itself sandboxed against the host:
  `CapDrop: ["ALL"]`, `SecurityOpt: ["no-new-privileges"]`, and — this is the part
  worth repeating — **no `Binds`, no `Mounts`, and never the Docker socket**. A
  runner container cannot see the host filesystem, cannot gain privileges, and
  cannot become a second path to the Engine API.

Do not relax any of this to make local testing more convenient. If you need to
reach `moira-runner` from off-box, put a TLS-terminating proxy in front of it
rather than binding it wide.

## Building and pinning the image

```bash
scripts/build-claude-runner-image.sh
```

This builds `deploy/claude-runner/Dockerfile`, runs `claude --version` inside the
result as a sanity check, and prints the image's content ID:

```
Image ID (copy this into moira-runner's image config):

  sha256:<64 hex characters>
```

`moira-runner` requires its configured image to be pinned by digest, not by a
mutable tag — a provisioned, credential-bearing container must not be able to
silently change under a tag like `:latest` pointing somewhere new tomorrow. Paste
the printed `sha256:...` value into `moira-runner`'s image configuration. Because
the image was built locally and never pushed to a registry, that value is a bare
Docker **image ID**, not a `repo@sha256:...` registry digest — verified: `docker
run someRepo@sha256:<that same id>` fails with "pull access denied" (no
`RepoDigests` exist for a purely local build), while `docker run sha256:<id>`
resolves correctly against the local daemon, because `moira-runner` and this
script talk to the same Docker daemon.

`moira-runner` accepts both forms as content-pinned — a bare `sha256:<64 hex>`
local image ID, or a `registry/name@sha256:<64 hex>` registry digest — and
rejects everything else (a bare repository name, `:latest`, or any other mutable
tag). See [`decisions-taken.md` §8](decisions-taken.md#8-moira-runners-image-reference-a-bare-local-image-id-counts-as-content-pinned)
for how that was settled and, importantly, its reversal condition:

> **The bare-ID form only means anything on the daemon that built it.** It holds
> exactly as long as `moira-runner` and the image share the same local Docker
> daemon, which is the only topology this document covers. **If you ever
> provision runners against a remote or shared Docker host** — a `DOCKER_HOST`
> pointing off-box, a managed container service, anywhere `moira-runner` does
> not share a filesystem and image store with wherever the image was built —
> a bare image ID from *this* machine is meaningless there. Push the image to a
> registry reachable from that host first, and configure `moira-runner` with
> that registry's real `name@sha256:...` digest instead.

The Claude CLI version is pinned inside the Dockerfile
(`CLAUDE_CODE_VERSION`, currently `2.1.233`), never `latest` — rebuilding on a
later date must not silently pick up a newer CLI release. Pass a version as the
script's first argument only to deliberately re-pin, and update the Dockerfile's
default alongside it so the two do not drift apart:

```bash
scripts/build-claude-runner-image.sh 2.1.240   # re-pin, deliberately
```

**Measured**, not assumed — actually built and actually run, on this machine, on
2026-08-16:

- `docker build` on `deploy/claude-runner/Dockerfile` succeeds and `claude
  --version` inside the resulting image prints `2.1.233 (Claude Code)`.
- The image's `User` is `node` (uid/gid 1000, not root), and `Entrypoint` is
  explicitly cleared (`ENTRYPOINT []`) rather than silently inheriting
  `node:24-alpine`'s own `docker-entrypoint.sh` — verified by `docker inspect`
  before and after adding that line.
- A container started the way `moira-runner` starts one — `docker run -dit
  --cap-drop=ALL --security-opt no-new-privileges --pids-limit 128 --memory
  256m <image> claude setup-token`, no binds, no mounts — gets a real tty
  (`docker logs` on it returns ~3KB of Ink UI output, matching the ~3044 bytes
  recorded in issue #272's evidence comment; a plain pipe gets 0) and the log
  contains the full `https://claude.com/cai/oauth/authorize?...` URL followed
  by `Paste code here if prompted >`.
- **Not measured**: capturing a *minted* token after pasting a real, valid
  authorization code. Only the shape of the prompt was verified here, the same
  boundary issue #272's own evidence comment draws. Prove this before depending
  on it for a first real login.
- ~~**Not measured**: the `moira-runner` binary itself.~~ **Superseded by R1
  (issue #273).** The binary now exists, and it was run against this image: it
  provisions a container, reaches `awaiting_authorization`, and returns the
  complete authorization URL. What still is *not* measured is the step after
  that — the CLI was never observed submitting a pasted code through the Engine
  API attach endpoint, which contradicts #272's evidence comment and is not yet
  explained. **`docs/moira-runner.md` is the authority on exactly what was and
  was not verified**; read it before depending on the paste half.

## Manually testing the image

Before wiring up `moira-runner`, you can exercise the image by hand through a
`docker-compose.yml` profile that mirrors the existing `dev-idp` (Keycloak)
profile:

```bash
docker compose --profile claude-runner up -d claude-runner
docker compose logs -f claude-runner                       # watch for the authorization URL
docker attach "$(docker compose ps -q claude-runner)"       # to paste a code by hand
docker compose --profile claude-runner rm --stop --force --volumes claude-runner
```

This is a manual test aid, not how provisioning works in production —
`moira-runner` creates and destroys these containers itself, one per provisioned
instance, over the Docker Engine API.

A sharp edge a previous recipe in this repo (the Keycloak one) got wrong on its
first pass, and that this profile deliberately avoids repeating:

- `docker compose --profile <name> up -d` **without naming the service** also
  starts every default-profile service — `postgres` and `redis` here — which on
  a machine that already ran `make up` are already running, and the command
  dies on "port is already allocated" before the profiled service is even
  reached. Always name the service: `up -d claude-runner`.
- Tearing it down with a bare `docker compose down` would also tear down that
  same `postgres` and `redis`. Stopping this service must stop this service
  only: `docker compose --profile claude-runner rm --stop --force --volumes
  claude-runner`.

Both commands above were run against this image while writing this document; the
first produced the authorization URL described above, and the second removed the
container cleanly without touching any other service.

## Configuring and starting the service

`moira-runner` is configured with (per the frozen control contract this image and
compose profile were built against):

- a bind address, defaulting to loopback (`127.0.0.1:8090`),
- a bearer token, compared in constant time, required on every route except
  `GET /healthz`,
- the pinned `image` reference described above,
- per-container resource limits (`Memory`, `NanoCpus`, `PidsLimit`) applied to
  every runner container it creates,
- a default TTL applied to new runners, after which the reaper force-removes
  them.

`moira-runner`'s exact configuration surface (environment variables or config
file keys) is defined in its own source, not here — check `moira-runner --help`
or its settings module once that binary lands. This document describes the
contract that surface must satisfy, not the literal variable names.

`moira-runner` is a plain binary, run the same way `moira` itself is for local
development (`cargo run --bin moira-runner`, not through `docker-compose.yml` —
neither the main API nor this service is containerised for local dev in this
repo; see `README.md`'s "Run Locally" section for the equivalent pattern). It
needs the host Docker socket reachable from wherever it runs and no other
container has it — this is the one process this whole design lets hold it, so
do not add a bind-mount of `/var/run/docker.sock` to any other service in
`docker-compose.yml`, including the `claude-runner` manual-test entry above,
which deliberately has none. Production deployment of `moira-runner` itself
(image, manifest, chart) is out of this document's scope.

Moira's own API process calls `moira-runner` server-to-server. It never asks the
console to do so, and the console never receives a bearer token or a Docker
socket path.

## Provisioning walkthrough

Provisioning is driven through Moira's admin API
(`/api/v1/admin/runners*`), which calls `moira-runner` on the operator's behalf.
The console surfaces this as a visible lifecycle rather than a single blocking
call, because step 3 below genuinely requires a human in a browser:

1. **Provision.** The operator creates a runner (a label and a TTL). Moira calls
   `moira-runner`, which starts a container from the pinned image with a
   daemon-allocated tty and no host access. State: `provisioning`.
2. **Open the URL.** Once the container's `claude setup-token` process has
   printed its authorization URL, the runner's state moves to
   `awaiting_authorization` and that URL becomes visible. The operator opens it
   in **their own browser** — nothing in this flow needs a port published or
   forwarded (see "no callback port" below) — and approves the request.
3. **Paste the code.** Anthropic's hosted page returns a short code to the
   operator. They paste it into the console; the console sends it to Moira,
   which forwards it to `moira-runner`, which writes it to the container's
   stdin over the Engine API's attach endpoint. State: `exchanging`.
4. **Token stored encrypted by Moira.** Once the CLI has exchanged the code,
   state moves to `ready` and the minted token becomes available, exactly once,
   from `moira-runner`. Moira retrieves it and stores it encrypted through the
   same credential-create path every other credential goes through. The console
   never sees the raw token at any point in this flow.
5. **Cleanup.** The runner can be deleted explicitly, or left to expire — the
   reaper force-removes any container past its `expires_at`, driven off Docker
   labels rather than an in-memory registry, so it also cleans up containers
   orphaned by a `moira-runner` restart.

### No callback port

The design question this feature started from — `claude setup-token`'s OAuth
callback listener lives inside the container, and the host browser cannot reach
a container-internal loopback listener — turned out not to apply. Measured
inside a container: `claude setup-token` does not open a local callback listener
at all. It uses Anthropic's own hosted redirect
(`https://platform.claude.com/oauth/code/callback`) and then prompts for the code
to be pasted back in, rather than waiting for a browser to hit a local port. The
browser only ever talks to `claude.com` / `platform.claude.com`; nothing about
this flow needs a container port published, forwarded, or reachable from the
host at all.

## Honest limits

Read this before provisioning more than one runner.

**N containers on a single Claude account is not N× capacity.** Rate limits
attach to the account, not to the process running against it. Provisioning five
runner containers against the same subscription does not give you five times the
throughput — it gives you one account's worth of throughput, spent from up to
five places, and deliberately fanning one subscription across parallel workers is
the exact pattern Anthropic's usage policy targets. **Meaningful multi-instance
use means one runner per real account or seat you legitimately hold** — five
runners are only five times the capacity if they are backed by five separate
subscriptions.

For capacity out of a *single* account relationship, the honest path is the
metered API key, not a fan of runner containers against one subscription — and an
API-key-backed provider should stay in the failover chain regardless of how many
runners you provision, exactly as `claude-subscription-sidecar.md` argues for the
sidecar shape.

Runner containers execute a credential-bearing CLI and need the same care as any
other credential surface:

- no shared volumes between instances (there are none by construction — the
  contract forbids `Binds`/`Mounts` entirely),
- no credential ever baked into an image layer (the image built here carries
  none; the token is captured off the live tty stream, never written to disk in
  the container or logged in full by `moira-runner`),
- per-instance cleanup on delete, so a deleted runner leaves nothing behind for
  the next one provisioned from the same image to inherit.

Anthropic's policy on subscription-token reuse by non-Claude-Code clients has
reversed multiple times within a single year (see
`plans/12-feature-expansion-brainstorm.md` §1, "R1 — policy volatility"). Nothing
about this container mechanism changes that risk; it only solves the tty problem.
Treat any subscription-backed credential this produces as one candidate in a
routing policy, never the only configured route to a model family.
