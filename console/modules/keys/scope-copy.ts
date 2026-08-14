// Scope string → catalog key.
//
// The mint form receives `offeredScopes` as raw Moira scope strings, because the
// server module is what decides which ones are offered (see
// `lib/consumer-keys.ts`). Something has to turn `moira:responses:create` into
// copy an operator can read, and a component may not render a raw scope string:
// `no-hardcoded-copy.test.tsx` would not catch it — it is data, not a literal —
// but an operator would be reading Moira's internal vocabulary off a form.
//
// A TABLE AND NOT A DERIVATION. `moira:rag-documents:read` → "Read documents" is
// not a transformation of the string; it is a decision about what that
// permission means to somebody who has never read the API. A `replace(/:/g, " ")`
// would produce something that looks translated and is not.
//
// An unmapped scope resolves to `null`, and the form drops it. That is the
// stricter of the two options on purpose: rendering the raw string would put
// `moira:` vocabulary on screen the moment `OFFERED_CONSUMER_SCOPES` grows an
// entry whose copy nobody wrote, and the missing copy would ship unnoticed. A
// dropped checkbox is visible in review; a raw scope string reads as intentional.

import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n";

const SCOPE_MESSAGE_KEYS: Readonly<Record<string, string>> = {
  "moira:responses:create": CONSOLE_MESSAGE_KEYS.keys_scope_responses_create,
  "moira:responses:stream": CONSOLE_MESSAGE_KEYS.keys_scope_responses_stream,
  "moira:responses:read": CONSOLE_MESSAGE_KEYS.keys_scope_responses_read,
  "moira:conversations:create": CONSOLE_MESSAGE_KEYS.keys_scope_conversations_create,
  "moira:conversations:read": CONSOLE_MESSAGE_KEYS.keys_scope_conversations_read,
  "moira:conversations:write": CONSOLE_MESSAGE_KEYS.keys_scope_conversations_write,
  "moira:memories:create": CONSOLE_MESSAGE_KEYS.keys_scope_memories_create,
  "moira:memories:read": CONSOLE_MESSAGE_KEYS.keys_scope_memories_read,
  "moira:rag-collections:read": CONSOLE_MESSAGE_KEYS.keys_scope_rag_collections_read,
  "moira:rag-documents:read": CONSOLE_MESSAGE_KEYS.keys_scope_rag_documents_read,
  "moira:usage:read": CONSOLE_MESSAGE_KEYS.keys_scope_usage_read,
};

/** The catalog key for `scope`, or `null` when nobody has written its copy. */
export function scopeMessageKey(scope: string): string | null {
  return SCOPE_MESSAGE_KEYS[scope] ?? null;
}
