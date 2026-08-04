FROM rust:1.97-bookworm AS builder

WORKDIR /app
COPY . .
RUN cargo build --release --locked

FROM gcr.io/distroless/cc-debian12:nonroot

WORKDIR /app
COPY --from=builder /app/target/release/moira /usr/local/bin/moira
COPY --from=builder /app/config/default.toml /app/config/default.toml

ENV MOIRA_SERVER__HOST=0.0.0.0
ENV MOIRA_SERVER__PORT=8080
ENV RUST_LOG=moira=info,tower_http=info

EXPOSE 8080

# Redundant against the `:nonroot` base tag, whose own `Config.User` is already `65532`
# — and stated anyway, for two reasons. A scanner reading only this file cannot see into
# the base image, so without this line every Dockerfile linter reports the container as
# running as root (semgrep's `dockerfile.security.missing-user-entrypoint` does exactly
# that, and the `sast` job in .github/workflows/ci.yml blocks on it); and if the base tag
# is ever changed to a variant that does not default to nonroot, the unprivileged user
# survives the edit instead of silently reverting to root.
#
# Numeric rather than the `nonroot` name, though the image defines both: Kubernetes can
# only verify `runAsNonRoot` against a numeric uid, and charts/moira pins `runAsUser:
# 65532` to match. `console/Dockerfile` states the same uid for the same reasons.
USER 65532:65532

ENTRYPOINT ["/usr/local/bin/moira"]
