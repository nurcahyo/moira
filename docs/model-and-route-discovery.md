# Model And Route Discovery

`GET /api/v1/models` lists active provider models visible through active routing policies for the caller.

`GET /api/v1/routes` lists active route definitions visible to the caller and summarizes capabilities observed from attached active models.

`GET /api/v1/capabilities` returns the caller application's execution policy booleans and limits:

- streaming
- vision
- tools
- structured output
- response persistence mode
- maximum input items
- maximum request bytes
- maximum output tokens

Discovery is authorization-filtered, but it is still not a provider health check and does not prove a live credential can execute successfully at that moment.

