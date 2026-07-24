# i18n Catalog Layout

This directory is the runtime source of truth for Moira's response copy.

## File Map

- `mod.rs`: catalog index, shared `I18nEntry`, lookup helpers, and catalog tests.
- `errors.rs`: all `moira.error.*` entries.
- `notices.rs`: all `moira.notice.*` entries.

## How To Extend

- Add a failure message to `errors.rs`.
- Add a success or readable notice message to `notices.rs`.
- If a new message family is needed, add a new file here and re-export it from `mod.rs`.
- Keep the docs mirror in `docs/i18n-response-catalog.json` synchronized with the Rust registry.

## Agent Shortcut

If you are updating translations:

- search `errors.rs` for error copy;
- search `notices.rs` for success/notice copy;
- search `mod.rs` for lookup helpers and catalog-level tests.
