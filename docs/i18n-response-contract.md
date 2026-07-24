# I18n Response Contract

Moira uses keyed API messages so the same payload can serve three audiences well:

- Next.js can translate by key.
- curl and Postman can read the English fallback directly.
- Operators and audit tooling can rely on stable keys for alerting and analytics.

## Wire Shape

Any user-visible API text should include:

- `message_key`: a stable translation identifier
- `message`: the default English fallback string
- `message_args`: interpolation values when the message needs placeholders

Failure responses should also include:

- `code`: a stable machine-readable error code
- `request_id`: the request correlation ID
- `details`: optional structured diagnostics for client-safe context

## How Clients Should Use It

### Next.js

Use `message_key` as the lookup key and fall back to `message` when a translation is missing.

```ts
const text = t(error.message_key, error.message_args, {
  defaultValue: error.message,
});
```

This keeps the UI localizable without making the backend depend on frontend translation files.

### curl

Use the `message` field directly when inspecting failures from the terminal.

```bash
curl -sS http://127.0.0.1:8080/api/v1/responses/resp_missing | jq -r '.error.message'
```

### Postman

Read `message` for the human summary and `message_key` for any localization or contract assertions in test scripts.

## Key Rules

- Keep keys stable and semantic.
- Use `moira.error.*` for failures and `moira.notice.*` for success messages or notices.
- Never emit freeform client-facing text without a key.
- Add every new key to the runtime registry in `src/i18n/catalog/`, then mirror it in `docs/i18n-response-catalog.json`.

## Catalog

The Rust registry is the source of truth for canonical English fallback strings:

- [src/i18n/catalog/mod.rs](../src/i18n/catalog/mod.rs)
- [src/i18n/catalog/errors.rs](../src/i18n/catalog/errors.rs)
- [src/i18n/catalog/notices.rs](../src/i18n/catalog/notices.rs)
- [docs/i18n-response-catalog.json](i18n-response-catalog.json)

Update both the registry and this guide whenever a new public message is introduced.
