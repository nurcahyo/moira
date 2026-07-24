# Moira i18n Response Contract

Use this skill when adding or documenting any user-visible API text in Moira. The goal is to keep the API readable on its own, while still making every message easy for the UI to translate.

## Core Contract

- Every user-visible API message must have a stable `message_key`.
- Every user-visible API message must also include an English fallback `message`.
- Structured interpolation data belongs in `message_args`.
- Failure responses must also include a machine-readable `code`.
- Do not emit freeform human text without a key when the text is meant for clients.

## Default Behavior

- English is the canonical fallback language.
- Next.js is responsible for locale selection and translation lookup.
- API consumers may always render `message` directly when no translation exists.
- `message_key` is the source of truth for UI copy, analytics, and consistency checks.

## Key Naming Rules

- Use the `moira.<group>.<name>` namespace.
- Use `moira.error.*` for failures and `moira.notice.*` for success messages or human-readable notices.
- Prefer short, stable, descriptive keys over endpoint-specific keys.
- Keep keys semantic, not sentence-shaped.
- Never reuse a key for a different meaning.

Examples:

- `moira.error.validation_failed`
- `moira.error.unauthorized`
- `moira.notice.response_completed`

## Authoring Rules

- Add the key to the runtime catalog in `src/i18n/catalog/` before using it in docs or API payload examples.
- Keep the English fallback concise, clear, and user-facing.
- Use `message_args` only for values the UI needs to interpolate.
- Do not put secrets, raw provider payloads, or internal diagnostics into the fallback message.
- Treat `message` as public API text, not a debug log.

## Response Shape Guidance

For failures, document the standard fields as:

- `code`
- `message_key`
- `message`
- `message_args`
- `request_id`
- `details` when applicable

For success payloads that expose human-readable text, use the same `message_key` + `message` pattern so Postman and curl can remain readable without frontend translation.

## Catalog Ownership

- Keep the registry in `src/i18n/catalog/`.
- Mirror the registry in `docs/i18n-response-catalog.json` for human-readable documentation.
- Update both when a new public message is added.
- If a key is deprecated, keep the old entry documented until all callers move off it.
- The Rust registry is the source of truth for backend validation and frontend vocabulary alignment.

## Review Checklist

- Is the message client-visible?
- Does it have a stable key?
- Is the English fallback included?
- Are any interpolation values represented in `message_args`?
- Can a curl/Postman user understand the response without the UI layer?
