# Conversation Persistence

The Phase 5 schema supports `none`, `metadata_only`, `plain_content`, and `encrypted_content` conversation persistence policies.

The current implementation persists bounded plain message content for conversations and direct text RAG versions. It does not persist protected internal instructions in conversation messages.

Stored message metadata includes role, message type, response/execution link, sequence number, content hash, size, token estimate, and safe metadata.

Deletion is soft by default and preserves audit metadata without message text in audit records.

