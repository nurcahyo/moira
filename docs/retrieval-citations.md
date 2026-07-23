# Retrieval Citations

Public responses include a `citations` list field.

Current behavior returns an empty list because retrieval context is not yet injected. Future retrieval should return response-level source provenance rather than fabricated inline sentence spans.

Safe citation fields include document ID, title, section, and source URI only where policy allows.

