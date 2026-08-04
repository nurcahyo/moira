# Needs human confirmation

Intake for decisions taken during plan execution that a human should double-check: ambiguous plan
wording, product choices not covered by `plans/CONVENTIONS.md` §0 D1–D7, security trade-offs made
unilaterally, and conflicts auto-resolved by a runner. Format is defined in
[`plans/RUNNER-PROMPT.md`](./plans/RUNNER-PROMPT.md) §10: `## <topic>` / **Context** / **What I
did** / **What I need confirmed**.

Ongoing work items live in [`TODO.md`](./TODO.md).

## Open decisions

None.

The eight decisions this file previously carried — from plans 02a and 02b — were signed off in
[issue #96](https://github.com/nurcahyo/moira/issues/96) and moved to
[`docs/decisions-taken.md`](./docs/decisions-taken.md), which records for each one the answer taken,
the evidence it was executed, and the condition under which it should be reversed.

Two of them are answered but **not finished**, and are tracked there rather than here:

- the 02a/02b plan-text grep guard, owned by [issue #82](https://github.com/nurcahyo/moira/issues/82);
- the post-deploy removal of the legacy `actor_fingerprint` fallbacks, which is gated on a
  production deploy plus 24 hours and has no owner yet.

Add new items under "Open decisions" above. When one is answered, move it to
`docs/decisions-taken.md` with its evidence and reversal condition instead of deleting it.
