# syntax=docker/dockerfile:1

# Moira console — separate deployable from the Rust service (CONVENTIONS §5).
#
# Multi-stage build: Bun installs and builds; the runtime stage is DISTROLESS
# and ships only the Next.js `standalone` output. No shell, no package manager,
# no Debian userland — non-root by construction. No secrets are baked into any
# layer; all runtime configuration is read from the environment at boot.
#
# ---------------------------------------------------------------------------
# WHY DISTROLESS, AND NOT node:24-slim
# ---------------------------------------------------------------------------
# CI runs aquasecurity/trivy-action with `exit-code: "1"` and
# `severity: CRITICAL,HIGH`. That is a hard gate, not advisory. Measured on
# this tree (linux/arm64, Trivy CRITICAL+HIGH, --scanners vuln):
#
#     node:24-slim runtime                        6 CRITICAL / 22 HIGH
#     gcr.io/distroless/nodejs24-debian12         1 CRITICAL /  5 HIGH
#     gcr.io/distroless/nodejs24-debian13         0 CRITICAL /  0 HIGH
#
# The delta is not patching, it is absence. 5 CRITICAL / 17 HIGH came from the
# Debian userland the app never executes (perl-base, zlib1g, util-linux,
# ncurses, gzip, libacl1); a further 1 CRITICAL / 3 HIGH came from the npm CLI
# that node:* images bundle at /usr/local/lib/node_modules/npm (its vendored
# tar, undici and brace-expansion). Distroless ships neither. This mirrors what
# Moira's own Rust image did by moving to gcr.io/distroless/cc-debian12:nonroot
# (36 CVEs -> 0).
#
# ---------------------------------------------------------------------------
# WHY debian13 AND NOT debian12
# ---------------------------------------------------------------------------
# The bookworm (debian12) distroless variant is NOT gate-clean: it carries a
# stale libssl3 3.0.18-1~deb12u2 worth 1 CRITICAL + 5 HIGH, all already fixed
# upstream in 3.0.19/3.0.20 (CVE-2026-31789, CVE-2026-28387/8/9/90,
# CVE-2026-45447). Those are unreachable in practice — Node statically links
# its OWN OpenSSL (process.versions.openssl reports 3.5.x), so the Debian
# libssl3 is baggage the server never loads — but Trivy scores images from
# package metadata, not reachability, so the gate fails on it regardless.
# Trixie (debian13) ships a current OpenSSL and scans 0/0, and its Node is
# v24.18.0, which matches console/.nvmrc exactly (bookworm's lags at v24.14.0).
#
# REVERSE THIS (drop back to debian12) only if trixie turns out to lack a
# shared library the app needs at runtime — the smoke test in the plan-08
# report covers boot + first request, so any such gap surfaces there.
#
# ---------------------------------------------------------------------------
# UID CONTRACT — DO NOT BREAK
# ---------------------------------------------------------------------------
# The distroless `:nonroot` variants run as uid/gid 65532. That is deliberate:
# charts/moira sets runAsUser 65532 / fsGroup 65532, and charts/moira-console
# matches. If this base image is ever swapped for one with a different non-root
# uid, the Helm chart securityContext MUST change in the same commit or the pod
# will fail to start on file ownership.
#
# ---------------------------------------------------------------------------
# BASE PINNING DECISION
# ---------------------------------------------------------------------------
# The base is pinned by TAG, not by digest, matching the existing Rust image.
# A floating `:nonroot` tag lets base-layer CVE fixes land without a code
# change, which is what keeps the hard Trivy gate green over time; digest
# pinning would convert every upstream base CVE into a red pipeline until a
# human bumps the hash. REVERSE THIS (pin by digest, plus a bot that bumps it)
# if the project adopts SLSA build-provenance or reproducible-build
# requirements, where a floating base tag is disqualifying.
#
# ---------------------------------------------------------------------------
# NO HEALTHCHECK — ON PURPOSE
# ---------------------------------------------------------------------------
# Kubernetes liveness/readiness probes own health (they hit /api/health).
# Moira removed its own HEALTHCHECK for the same reason, and a distroless image
# has no shell, so a CMD-form HEALTHCHECK could not execute here anyway.

ARG BUN_VERSION=1.3.14
# Keep in step with console/.nvmrc and package.json `engines.node` (>=24 <25).
ARG NODE_MAJOR=24
# Debian suite of the distroless runtime. See "WHY debian13" above before
# changing this — debian12 does not pass the Trivy gate.
ARG DEBIAN_SUITE=debian13

# ---- deps: install exact, locked dependencies -----------------------------
FROM oven/bun:${BUN_VERSION} AS deps
WORKDIR /app

# Only the manifest + lockfile, so this layer caches independently of source.
COPY package.json bun.lock ./
RUN bun install --frozen-lockfile

# ---- build: compile the Next.js app ----------------------------------------
FROM oven/bun:${BUN_VERSION} AS build
WORKDIR /app

COPY --from=deps /app/node_modules ./node_modules
COPY . .

ENV NEXT_TELEMETRY_DISABLED=1
RUN bun run build

# Drop the image-optimizer stack from the traced standalone output.
#
# `sharp` is an optionalDependency of next@16 ("sharp": "^0.34.5"), so Next
# traces it into .next/standalone whether or not the app renders a single
# <Image>. It costs ~34 MB — two thirds of the runtime node_modules — and after
# the move to distroless it is the ONLY remaining CRITICAL/HIGH finding:
# GHSA-f88m-g3jw-g9cj against sharp 0.34.5, fixed in 0.35.0, a version this
# lockfile cannot reach while next's own range caps at ^0.34.5.
#
# The console renders no optimized images, so the optimizer is dead weight and
# removing it is the honest fix. Trivy suppression via --ignore-unfixed is not,
# and would not even apply here, since a fixed version exists upstream.
#
# The grep below makes that assumption self-enforcing: the day someone imports
# next/image, this BUILD fails with instructions, instead of the container
# returning 500s from /_next/image in production.
#
# REVERSE THIS BLOCK when either becomes true:
#   * next widens its sharp range to >= 0.35.0 — then delete this RUN entirely;
#   * the console genuinely needs <Image> — then either set
#     `images: { unoptimized: true }` in next.config.ts (keeps the optimizer,
#     and sharp, out of the picture), or add a package.json `overrides`
#     entry pinning sharp >= 0.35.0 and delete this RUN.
RUN set -eu; \
    if grep -rqs \
        -e 'from "next/image"' \
        -e "from 'next/image'" \
        -e 'require("next/image")' \
        -e 'next/image"' \
        app components modules; then \
      echo "ERROR: next/image is now imported, but this Dockerfile strips sharp." >&2; \
      echo "       See the comment above this RUN for the two supported fixes." >&2; \
      exit 1; \
    fi; \
    rm -rf .next/standalone/node_modules/sharp \
           .next/standalone/node_modules/@img \
           .next/standalone/node_modules/detect-libc

# Pre-create the incremental-cache directory inside the standalone tree so it
# exists owned by 65532. charts/moira-console runs the pod with
# `readOnlyRootFilesystem: true` and mounts an emptyDir over .next/cache; the
# directory existing in the image keeps a plain `docker run --read-only` (and
# any write that happens outside that mount) from failing on ENOENT.
RUN mkdir -p .next/standalone/.next/cache

# ---- runtime: distroless, non-root, standalone server ----------------------
# ENTRYPOINT of this base is already ["/nodejs/bin/node"], so CMD must NOT
# repeat `node` — it carries the script path only.
FROM gcr.io/distroless/nodejs${NODE_MAJOR}-${DEBIAN_SUITE}:nonroot AS runtime
WORKDIR /app

ENV NODE_ENV=production \
    NEXT_TELEMETRY_DISABLED=1 \
    PORT=3000 \
    HOSTNAME=0.0.0.0

# `output: "standalone"` (next.config.ts) traces the minimal set of files a
# production server needs — no full node_modules tree in the final image.
# There is no shell here, so ownership must be set on the COPY itself; there is
# no useradd/groupadd either, hence the numeric uid:gid.
COPY --from=build --chown=65532:65532 /app/public ./public
COPY --from=build --chown=65532:65532 /app/.next/standalone ./
COPY --from=build --chown=65532:65532 /app/.next/static ./.next/static

# Redundant with the `:nonroot` tag, and intentionally so: this keeps the
# container non-root even if the base tag is ever changed to a rootful variant.
USER 65532:65532

EXPOSE 3000

CMD ["server.js"]
