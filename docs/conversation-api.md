# Conversation API

Public endpoints:

- `POST /api/v1/conversations`
- `GET /api/v1/conversations`
- `GET /api/v1/conversations/{conversation_id}`
- `PATCH /api/v1/conversations/{conversation_id}`
- `DELETE /api/v1/conversations/{conversation_id}`
- `POST /api/v1/conversations/{conversation_id}/archive`
- `POST /api/v1/conversations/{conversation_id}/restore`
- `GET /api/v1/conversations/{conversation_id}/messages`
- `POST /api/v1/conversations/{conversation_id}/messages`

Responses support optional conversation attachment:

```json
{
  "conversation": { "id": "conv_..." },
  "input": []
}
```

or explicit creation:

```json
{
  "conversation": { "create": true, "title": "New discussion" },
  "input": []
}
```

Idempotent response replays do not create duplicate conversation messages.

