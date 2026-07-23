FROM rust:1.97-bookworm AS builder

WORKDIR /app
COPY . .
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --gid root --home /nonexistent --shell /usr/sbin/nologin moira

WORKDIR /app
COPY --from=builder /app/target/release/moira /usr/local/bin/moira
COPY --from=builder /app/config/default.toml /app/config/default.toml

ENV MOIRA_SERVER__HOST=0.0.0.0
ENV MOIRA_SERVER__PORT=8080
ENV RUST_LOG=moira=info,tower_http=info

USER 10001
EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl --fail --silent http://127.0.0.1:8080/health/live >/dev/null || exit 1

ENTRYPOINT ["/usr/local/bin/moira"]
