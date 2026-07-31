# modules/ — organisms

This directory holds the console's **organisms**: feature-aware,
composed UI modules that own a slice of a page (per
`plans/CONVENTIONS.md` §6 — e.g. `SetupWizard`, `ProviderTable`,
`CredentialForm`, `AuditLogPanel`).

## What lives here

- `signIn/SignInPanel.tsx` — the sign-in surface. An organism because the
  layering test forbids atoms and molecules from calling `fetch(`, from
  importing `next/navigation`, and from importing any `auth`-matching
  specifier. It is also the first `"use client"` file in the repository.
- `secrets/OnceOnlySecretModal.tsx` — the once-only invitation token. The
  single file allow-listed by
  `tests/unit/architecture/no-secret-props.test.ts`, and the only place in
  the console permitted to hold a plaintext secret in a prop.

Ground rules (from `plans/CONVENTIONS.md` §6), each now enforced by a test
rather than by convention:

- Organisms may compose molecules (`console/components/molecules/`) and
  atoms (`console/components/atoms/`), and may call the Moira client — but
  only from a SERVER organism. A `"use client"` organism may import
  `lib/errors.ts`, `lib/types.ts`, `lib/moira-keys.ts` and `lib/i18n/**`
  and nothing else from `lib/`; everything else there is credential-carrying
  or declares itself server-only, and
  `tests/unit/architecture/layer-dependencies.test.ts` enforces it.
- Dependency direction is one-way: **pages → organisms → molecules →
  atoms**. `modules/** ↛ app/**` is enforced by the same test, with a
  scanned-file floor so the rule cannot go vacuous by the directory
  emptying.
- Secrets never descend past the page/server boundary.
  `no-secret-props.test.ts` scans every `*Props` interface here, allows
  exactly ONE file to name a secret, and additionally asserts that the
  allowed file never forwards it to a child component.
- Every organism ships a unit test. Assert rendered copy against
  `CONSOLE_CATALOG[key].message`, never against an English literal — a
  literal assertion passes whether or not `t()` was ever called.
