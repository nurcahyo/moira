# Memory Retrieval

Implemented in `find_memory_candidates` (`src/infra/repositories/conversation.rs`) and ranked by
`src/orchestration/retrieval.rs`. Plan 11 Sub-Phase C.

Memory is always treated as user-context data, never as instructions and never as authorization
material. See [`rag-security.md`](./rag-security.md) for the structural boundary.

## Writing the index

`memory_embeddings` is written by `ConversationService::create_memory` when
`application_embedding_policies.memory_embeddings_enabled` is true. The write happens **after**
the memory row is committed, not inside its transaction: an embedding failure must not lose a
memory the caller successfully stored.

An unembedded memory is still a valid memory. It is simply not semantically retrievable, and the
absence of the `memory_embeddings` row is how that stays visible — there is no status column
claiming otherwise.

## The isolation predicate

`application_id` is required in **every** arm. There is no path that crosses applications.
Within an application, each `memory_scope` value is matched against exactly the isolation its own
definition implies:

| `memory_scope` | Visible to |
|---|---|
| `application` | the whole application, across tenants and users — deliberately |
| `tenant_application` | the matching `external_tenant_id` only |
| `user_application`, `conversation` | the matching tenant **and** user |

This is a deliberate departure from the flat
`coalesce(external_tenant_id,'') = coalesce($3,'')` predicate the plan sketched. The flat form is
strictly isolating but makes `memory_scope = 'application'` unreachable for any caller carrying
an `external_user_id`, because such rows have a null user by construction
(`memory_records_scope_valid`). An application-scoped memory nothing can retrieve is not a safety
property; it is a dead column.

Both directions are pinned by named tests in `tests/retrieval_cross_tenant_isolation.rs`:
`application_scoped_memories_are_deliberately_shared_across_tenants` fails if the exception is
removed, and the tenant/user cases fail if it is widened.

**Reversal condition:** collapse to the flat predicate if product decides application-scoped
memories must not cross tenants — at which point the scope value itself should be removed rather
than left meaning nothing.

## Where the filter runs

Inside the candidate query, in the same statement as the `order by <=>`. Never as a post-fetch
filter in Rust. Filtering afterwards computes a global nearest-neighbour ordering over every
tenant's rows and then discards the ones that do not belong to the caller, which leaks the shape
of other tenants' corpora through counts and timing even when no content is returned.
`retrieval_run_counts_never_include_out_of_scope_candidates` is the test that catches that
specific mistake — the returned rows would be correct, only the counts betray it.
