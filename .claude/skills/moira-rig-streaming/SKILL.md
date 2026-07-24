---
name: moira-rig-streaming
description: Bridge rig-core 0.40 streaming completions to Moira's runtime event pipeline and Axum SSE contract. Covers obtaining a StreamingCompletionResponse from a Rig CompletionModel, exhaustive StreamedAssistantContent handling, boxing and pinning with the correct Send/Unpin/'static bounds, draining to capture final Usage, translating chunks into RuntimeStreamItem and RuntimeEventEnvelope, mid-stream error classification, cancellation and idle timeouts, ordering and flush guarantees, and testing streams without a network. Use when adding or changing streaming execution, adding a streamed chunk kind, touching the stream idle timeout, backpressure, cancellation, usage capture, tool-call deltas, reasoning deltas, the RuntimeStreamItem or RuntimeEventType enums, the public SSE event mapping, or any test that exercises a streamed provider response.
---

# Moira Rig Streaming

Read and follow `../../../.agents/skills/moira-rig-streaming/SKILL.md` completely. That canonical workflow is shared by Codex and Antigravity and is authoritative for this repository.
