# RAG Security

Retrieved documents are untrusted data.

Future context assembly must frame RAG chunks as reference material, not instructions, and must never allow retrieved content to grant tools, scopes, credentials, or identity.

Current ingestion rejects unsupported MIME/source types and secret-like content. Remote URL ingestion is not enabled, avoiding SSRF risk until the full URL policy is implemented.

