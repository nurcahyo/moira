# Memory Consent

Supported consent modes:

- `disabled`
- `explicit_only`
- `application_managed`
- `automatic_with_user_controls`

## There are two consent columns

`application_memory_policies.consent_mode` and
`application_conversation_policies.memory_consent_mode` are **independent** columns over the same
four values. Both default to `'explicit_only'`, and nothing in the schema makes them agree.

- The **explicit memory API** (`POST /api/v1/memories`) reads
  `application_memory_policies.consent_mode` and refuses under `disabled`.
- **Automatic extraction** (plan 11 Sub-Phase F) reads **both**, and takes the more restrictive
  of the two: either column at `disabled` refuses the run entirely; either at `explicit_only`
  produces `status = 'candidate'` rows that retrieval never serves. See
  `docs/memory-extraction.md`.

An operator who wants extraction off for an application can therefore set either column, and an
operator who turns one on has not accidentally turned extraction on while the other still
withholds consent.

Automatic retrieval remains a policy field that is off by default.

Conversation history consent is separate from long-term memory consent.
