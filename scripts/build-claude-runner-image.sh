#!/usr/bin/env bash
# Build the moira-claude-runner image (issue #272, R4) and print the image ID
# moira-runner's config needs — see docs/claude-runners.md.
#
# **Why an image ID and not a tag.** moira-runner's control contract requires
# the container it creates to come from a content-pinned reference, not a
# mutable tag: a tag like `moira-claude-runner:latest` can point at a different
# image tomorrow with no record of the change, which is exactly the property a
# provisioned, credential-bearing container must not have. `docker inspect
# --format '{{.Id}}'` after the build gives the sha256 digest of the actual
# image content. That digest is what changes if — and only if — the pinned
# `CLAUDE_CODE_VERSION` build arg or `deploy/claude-runner/Dockerfile` changes.
#
# **Why not a registry digest (`name@sha256:...`).** That form only resolves
# once an image has actually been pushed to (or pulled from) a registry, which
# records it in `RepoDigests`; a purely local build has none. Verified: `docker
# run someRepo@sha256:<local image id>` fails with "pull access denied" even
# when that id is a real local image, while `docker run sha256:<id>` (bare,
# unprefixed) resolves correctly against the local daemon — proven by actually
# running `claude --version` through it. moira-runner and this script talk to
# the same Docker daemon (see runner-contract.md's trust chain), so the bare
# image ID is what its `image` config value should hold. If you push this image
# to a registry instead, use that registry's own `name@sha256:...` digest.
#
# Usage: scripts/build-claude-runner-image.sh [claude-code-version]
#   claude-code-version   defaults to the version pinned in the Dockerfile's
#                          CLAUDE_CODE_VERSION ARG default (do not pass this to
#                          silently float onto a newer release — pass it only
#                          to deliberately re-pin, then update the Dockerfile
#                          default too so the two do not drift apart).

set -euo pipefail
cd "$(dirname "$0")/.."

DOCKERFILE=deploy/claude-runner/Dockerfile
BUILD_CONTEXT=deploy/claude-runner
IMAGE_TAG=moira-claude-runner:local

if [ ! -f "$DOCKERFILE" ]; then
    printf 'build    FAILED — %s not found (run from the repo root or check the path)\n' "$DOCKERFILE" >&2
    exit 1
fi

# A single string, not an array: the macOS-shipped bash (3.2) mishandles an
# empty array expansion under `set -u` ("unbound variable"), and this value
# never contains whitespace, so word-splitting it unquoted below is safe.
BUILD_ARG=""
if [ $# -ge 1 ]; then
    printf 'build    pinning CLAUDE_CODE_VERSION=%s (overrides the Dockerfile default — update it too if this is a deliberate re-pin)\n' "$1"
    BUILD_ARG="--build-arg=CLAUDE_CODE_VERSION=$1"
fi

printf 'build    %s from %s (context %s)\n' "$IMAGE_TAG" "$DOCKERFILE" "$BUILD_CONTEXT"
docker build -f "$DOCKERFILE" -t "$IMAGE_TAG" $BUILD_ARG "$BUILD_CONTEXT"

IMAGE_ID=$(docker inspect "$IMAGE_TAG" --format '{{.Id}}')

printf 'verify   claude --version inside the built image\n'
CLAUDE_VERSION=$(docker run --rm "$IMAGE_ID" claude --version)
printf '         %s\n' "$CLAUDE_VERSION"

printf '\n'
printf 'Image ID (copy this into moira-runner'\''s image config):\n\n'
printf '  %s\n\n' "$IMAGE_ID"
printf 'Tag %s also points at it locally for convenience, but the tag is mutable —\n' "$IMAGE_TAG"
printf 'the ID above is the value the runner config should hold. See docs/claude-runners.md.\n'
