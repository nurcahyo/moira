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

- **Automatic extraction** (plan 11 Sub-Phase F) reads **both**, and takes the more restrictive
  of the two: either column at `disabled` refuses the run entirely; either at `explicit_only`
  produces `status = 'candidate'` rows that retrieval never serves. See
  `docs/memory-extraction.md`.
- **`ConversationRecord.memory_behavior`**, returned by `GET /api/v1/conversations` and
  `GET /api/v1/conversations/{id}`, reports the same stricter-of-the-two value, so what a caller
  is told matches what is enforced. It reads `policy_controlled` on the responses of endpoints
  whose query does not select both columns.
- The **explicit memory API** (`POST /api/v1/memories`) reads
  `application_memory_policies.consent_mode` alone, and refuses under `disabled`. This is the one
  deliberate exception: a manual memory is written at `user_application` scope and carries no
  conversation id, so `application_conversation_policies` is not describing it. Its own switch is
  `application_memory_policies.manual_memory_enabled`.

An operator who wants extraction off for an application can therefore set either column, and an
operator who turns one on has not accidentally turned extraction on while the other still
withholds consent.

## The rule lives in one place, and it is not in SQL

`MemoryConsentMode::stricter_of` (`src/domain/conversation.rs`) is the only expression of "which
of the two columns wins". Everything that needs a consent decision calls it — extraction through
`effective_extraction_status`, and `memory_behavior` through `effective_memory_behavior` in
`src/infra/pg_rows.rs`.

That it lives on the *domain* type rather than in the application layer is finding F30's actual
lesson. The rule was originally an application-layer function, so the query that needed the same
answer could not call it and wrote its own — `coalesce(mp.consent_mode, 'explicit_only')`, one
column, in six words of SQL, where no amount of Rust discipline was in its way. `conversation_select`
now selects both columns raw and decides nothing, and the single-column mapping
(`status_for_consent_mode`) is private, so the easy way to get consent wrong from one column no
longer exists.

**Nothing in the schema enforces this.** A cross-table `CHECK` is not something Postgres offers,
and the two columns are deliberately independent — an operator may tighten either. So the barrier
is the one above, plus
`the_reported_memory_behavior_is_the_stricter_of_the_two_consent_columns` and
`explicit_only_on_the_conversation_policy_alone_still_withholds_the_memory` in
`tests/memory_extraction.rs`, both of which set the two columns to **different** values. A test
that sets them to the same value cannot see a reader consulting one of them, which is exactly how
`memory_behavior` shipped.

Automatic retrieval remains a policy field that is off by default.

Conversation history consent is separate from long-term memory consent.
