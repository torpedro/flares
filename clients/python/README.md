# Flare Python client

Requires Python 3.11+. Install the built wheel with `pip install flare_client-*.whl`, or install from this directory with `uv pip install .`.

```python
from flare_client import Client

with Client("http://localhost:8000", "your-api-token") as client:
    result = client.open_issue("backup", title="Backup failed", severity="critical")
    print(result.issue.status, result.notification.status)
    client.close_issue("backup")
```

```python
from flare_client import AsyncClient


async def notify():
    async with AsyncClient("http://localhost:8000", "your-api-token") as client:
        return await client.alert("Backup", "Finished", idempotency_key="backup-run-123")
```

Both clients have the same operations: `open_issue`, `close_issue`, `get_issue`, `list_issues`, `alert`, `get_delivery`, `register_heartbeat`, `check_in`, `list_heartbeats`, `delete_heartbeat`, `health`, `readiness`, and `metrics`. Responses are typed Pydantic models, except `metrics` (text) and `delete_heartbeat` (`None`). Models and enums are exported from `flare_client`. Request options match the HTTP API; list options are `status`, `limit=100`, and `offset=0`.

Use `timeout=15.0` to customize HTTPX's network-operation timeouts. Keep a client open to reuse connections; context managers close it, or call `close()` / `await aclose()` explicitly. Library configuration is passed directly; the server and CLI retain their YAML configuration.

Catch `ValidationError`, `TransportError` (with `timed_out`), `HTTPError` (with `status_code`), or `DecodeError`; all inherit `FlareError`. Error messages do not include secrets or provider response bodies. A successful HTTP response with notification status `failed` is a normal result, not an exception. `pending` means accepted, not delivered. Requests never retry or follow redirects automatically. Reuse an explicit alert idempotency key after an uncertain outcome.

Development (from this directory): `uv sync --locked`, `uv run pytest tests`, `uv build`. Run the repository contract suite after building the server and Rust contract driver; see the root README.
