# modules/ — organisms

This directory holds the console's **organisms**: feature-aware,
composed UI modules that own a slice of a page (per
`plans/CONVENTIONS.md` §6 — e.g. `SetupWizard`, `ProviderTable`,
`CredentialForm`, `AuditLogPanel`).

**Nothing lives here yet.** This is the workspace scaffold
(`chore/console-workspace-scaffold`); organisms are feature code and are
out of scope for it. They are added by **plan 08**
(`plans/08-nextjs-console-google-oauth.md`), once plan 07 lands Moira's
identity contract that the console's auth/setup organisms bind to.

Ground rules for whoever adds the first organism here (from
`plans/CONVENTIONS.md` §6):

- Organisms may call server actions and the Moira client, and may
  compose molecules (`console/components/molecules/`) and atoms
  (`console/components/atoms/`).
- Dependency direction is one-way: **pages → organisms → molecules →
  atoms**. An organism must never be imported by a molecule or atom.
- Secrets never descend past the page/server boundary — a system key or
  decrypted credential must never be passed as a prop into an organism,
  since organisms can render client-side.
- Every organism ships a unit test, plus an e2e test through the page
  that hosts it.

See `console/modules/.gitkeep` (this file's sibling) — it exists only so
this otherwise-empty directory is tracked by git ahead of plan 08.
