---
name: moira-openapi
description: Generate and maintain Moira's Axum OpenAPI 3.1 contract. Use when adding, removing, renaming, or changing HTTP routes, handler request or response DTOs, query or path parameters, authentication, headers, status codes, SSE behavior, metrics output, or API documentation.
---

# Moira OpenAPI

Keep route registration and documentation in the same `utoipa_axum::OpenApiRouter`.

## Workflow

1. Read `skills/moira-project-structure/SKILL.md` completely.
2. Inspect the handler, its domain DTOs, service errors, authentication call, status codes, and response headers.
3. Add or update `ToSchema` on JSON DTOs and `IntoParams` on query DTOs. Keep reusable API types in `src/domain`; keep HTTP-only error and extractor details at the HTTP boundary.
4. Add or update `#[utoipa::path]` on every handler. Document:
   - the exact method and Axum path;
   - path, query, and behaviorally supported header parameters;
   - request bodies and actual success statuses;
   - JSON, `text/event-stream`, `text/plain`, or `text/html` content types;
   - `ETag`, `If-Match`, `Idempotency-Key`, and `X-Request-Id` where implemented;
   - typed error responses and accepted security schemes.
   Keep the global `X-Request-Id` request parameter and response header enrichment in
   `src/http/openapi.rs::finalize_document`; do not duplicate it on individual operations.
5. Register the handler with `utoipa_axum::routes!` in `src/http/mod.rs`. Group handlers only when they share one path and use distinct methods.
6. If two URLs share service logic, give each URL a distinct annotated handler wrapper and operation ID.
7. Preserve `MOIRA_DOCS__EXPOSE_ADMIN`: generate the full contract internally, filter `/api/v1/admin/**` publicly, and require `moira:admin` before returning the full document.
8. Keep plaintext provider secrets, API-key hashes, ciphertext, nonces, peppers, decrypted JWT material, embeddings, protected instructions, and internal prompts out of schemas and examples.
9. Update `docs/openapi.md`, relevant API docs, and completed OpenAPI TODOs.

## Validation

Run:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The OpenAPI tests must prove that all registered paths and methods are present, operation IDs are unique, local schema references resolve, public filtering works, and representative parameters, status codes, security schemes, and content types remain correct.

Do not reintroduce hand-built JSON path catalogs or register undocumented Axum routes outside `OpenApiRouter`.
