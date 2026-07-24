---
name: moira-rig-agents-rag
description: Decide whether Moira should use a rig-core 0.40 Agent, Extractor, or vector-store abstraction at all, and wire it correctly when the answer is yes. Covers AgentBuilder configuration (preamble, static and dynamic context, tools, turn budget, output schema, memory), the Prompt/Chat/TypedPrompt traits and PromptRequest, Extractor and OutputMode structured output with schemars, embeddings (EmbeddingModel, EmbeddingsBuilder, Embed derive), vector stores (VectorStoreIndex, InMemoryVectorStore, the pgvector path over Moira's own tables instead of rig-postgres), end-to-end RAG wiring, and the removal of the pipeline module in 0.40.0. Use when considering an Agent, Extractor, embeddings, a vector store, retrieval-augmented prompting, conversation memory, memory extraction, conversation summarisation, or RAG ingestion anywhere in Moira, and read it before adding any of these — the default answer for the public response path is to stay at the CompletionModel level.
---

# Moira Rig Agents and RAG

Read and follow `../../../.agents/skills/moira-rig-agents-rag/SKILL.md` completely. That canonical workflow is shared by Codex and Antigravity and is authoritative for this repository.
