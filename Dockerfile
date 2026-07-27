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

ENTRYPOINT ["/usr/local/bin/moira"]
