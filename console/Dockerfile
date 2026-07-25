# syntax=docker/dockerfile:1

# Moira console — separate deployable from the Rust service (CONVENTIONS §5).
# Multi-stage build: Bun 1.3.14 installs deps and builds; the runtime stage
# only ships the Next.js `standalone` output on a minimal Node 24 image,
# running as a non-root user. No secrets are baked into any layer — all
# runtime configuration is read from the environment / Moira at boot.

ARG NODE_VERSION=24.18.0
ARG BUN_VERSION=1.3.14

# ---- deps: install exact, locked dependencies -----------------------------
FROM oven/bun:${BUN_VERSION} AS deps
WORKDIR /app

COPY package.json bun.lock ./
RUN bun install --frozen-lockfile

# ---- build: compile the Next.js app ----------------------------------------
FROM oven/bun:${BUN_VERSION} AS build
WORKDIR /app

COPY --from=deps /app/node_modules ./node_modules
COPY . .

ENV NEXT_TELEMETRY_DISABLED=1
RUN bun run build

# ---- runtime: minimal, non-root, standalone server -------------------------
FROM node:${NODE_VERSION}-slim AS runtime
WORKDIR /app

ENV NODE_ENV=production \
    NEXT_TELEMETRY_DISABLED=1 \
    PORT=3000 \
    HOSTNAME=0.0.0.0

RUN groupadd --system --gid 1001 nodejs \
    && useradd --system --uid 1001 --gid nodejs nextjs

# `output: "standalone"` (next.config.ts) traces the minimal set of files
# a production server needs — no full node_modules tree in the final image.
COPY --from=build --chown=nextjs:nodejs /app/public ./public
COPY --from=build --chown=nextjs:nodejs /app/.next/standalone ./
COPY --from=build --chown=nextjs:nodejs /app/.next/static ./.next/static

USER nextjs

EXPOSE 3000

CMD ["node", "server.js"]
