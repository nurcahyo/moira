# Memory Architecture

Memory is not conversation history. Memory records are reusable facts, preferences, goals, decisions, or constraints retained under explicit policy.

Memory scopes:

- `conversation`
- `user_application`
- `tenant_application`
- `application`

The public explicit memory API currently creates `user_application` memories only, and only when memory policy enables manual memory.

