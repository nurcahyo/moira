# The image every provisioned Claude runner container runs (issue #272, R4).
#
# `moira-runner` (see the frozen control contract this image was built against,
# and `docs/claude-runners.md`) starts one of these per provisioned instance with
# `Tty: true`, `OpenStdin: true`, no `Binds`/`Mounts`, `CapDrop: ["ALL"]`, and
# `SecurityOpt: ["no-new-privileges"]`. Everything below is written for that
# caller, not for an interactive `docker run` on a workstation.
#
# Base measured working on 2026-08-15 (see issue #272's evidence comment):
# `npm i -g @anthropic-ai/claude-code` on `node:24-alpine` -> `claude --version`
# -> `2.1.233 (Claude Code)`. Do not swap the base image without re-measuring —
# the CLI's own install docs target glibc/Debian first, and Alpine's musl libc
# is the one combination actually proven here.
FROM node:24-alpine

# PINNED, never `latest`. moira-runner's config carries this image by content
# digest (`image` must contain an `@sha256:...`, see the runner control
# contract), and scripts/build-claude-runner-image.sh prints that digest after
# every build — but the version pin here is the second half of "not a moving
# target": a rebuild from the same Dockerfile on the same day must not silently
# pick up a newer CLI release just because `latest` moved under it.
ARG CLAUDE_CODE_VERSION=2.1.233

RUN npm install --global "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" \
    && npm cache clean --force \
    && claude --version

# node:24-alpine already ships a non-root `node` user (uid/gid 1000) with
# /home/node pre-owned by it — verified by actually running the base image
# (`id node` -> uid=1000(node), `ls -ld /home/node` -> owned by node:node).
# No `useradd`/`adduser` needed, and no reason to run this as root: the
# container mints an OAuth-derived, credential-bearing token
# (`claude setup-token`), and root inside it would be root for no benefit.
#
# HOME is set explicitly rather than left to the /etc/passwd default so it
# survives however moira-runner's Engine API call shapes the container's Env —
# the CLI persists its session/config under $HOME (~/.claude.json, ~/.claude/)
# and needs a HOME that both resolves and is writable by the user running it.
ENV HOME=/home/node
USER node
WORKDIR /home/node

# No secrets here and no build ARG carries one. CLAUDE_CODE_VERSION above is a
# public npm version string, not a credential. The token this container mints
# is captured by moira-runner from the container's tty stream after the
# operator authorizes in their own browser (see docs/claude-runners.md) — it
# never touches an image layer, a build arg, or an environment variable baked
# in here.
#
# Deliberately NO ENTRYPOINT — and explicitly cleared, not just omitted.
# `node:24-alpine` itself sets `ENTRYPOINT ["docker-entrypoint.sh"]`, which a
# bare Dockerfile silently inherits (verified: `docker inspect` on an image
# built from this file without the line below still showed that entrypoint).
# That particular script `exec`s straight into "$@" so it does not, in
# practice, hold onto the tty — but the runner contract creates every
# container with an explicit argv (default `["claude","setup-token"]`) and a
# daemon-allocated tty, and relying on an inherited upstream script to keep
# behaving that way across base-image updates is exactly the kind of
# assumption this repo has been burned by before. `ENTRYPOINT []` removes it,
# so `Cmd` from the Engine API create call is what actually runs, full stop.
ENTRYPOINT []
CMD ["claude", "--version"]
