# Flares Python client

Requires Python 3.11+. Install the built wheel with `pip install flares_client-*.whl`, or install from this directory with `uv pip install .`.

```python
from flares_client import Client

with Client("http://localhost:8000", "your-api-token") as client:
    result = client.open_issue("backup", title="Backup failed", severity="critical")
    print(result.issue.status, result.notification.status)
    client.close_issue("backup")
```

```python
from flares_client import AsyncClient


async def notify():
    async with AsyncClient("http://localhost:8000", "your-api-token") as client:
        return await client.alert("Backup", "Finished", idempotency_key="backup-run-123")
```

Both clients have the same operations: `open_issue`, `close_issue`, `get_issue`, `list_issues`, `alert`, `get_delivery`, `register_heartbeat`, `check_in`, `list_heartbeats`, `delete_heartbeat`, `health`, `readiness`, and `metrics`. Responses are typed Pydantic models, except `metrics` (text) and `delete_heartbeat` (`None`). Models and enums are exported from `flares_client`. Request options match the HTTP API; list options are `status`, `limit=100`, and `offset=0`.

Use `timeout=15.0` to customize HTTPX's network-operation timeouts. Keep a client open to reuse connections; context managers close it, or call `close()` / `await aclose()` explicitly. Direct constructors use only the supplied settings.

Both classes can also load the same YAML file as the CLI:

```python
with Client.from_config("/path/client.yaml") as client:
    print(client.health())


async def health():
    async with AsyncClient.from_default_config() as client:
        return await client.health()
```

`from_config(path)` selects exactly that file; `from_default_config()` opts into
`$XDG_CONFIG_HOME/flares/client.yaml` when XDG is absolute, otherwise
`$HOME/.config/flares/client.yaml`, then `/etc/flares/client.yaml`. Empty/relative
XDG values are ignored; missing/empty HOME skips that location. Files are never
merged; an invalid or unreadable selected file fails without fallback. Explicit
paths never fall back. Both classes offer both constructors, which load files
synchronously and make no API requests.

YAML accepts `base_url` (default `http://127.0.0.1:8000`), required `api_token`, and
`timeout` (default `15s`; whole-number durations with `s`, `m`, `h`, `d`, or `w`).
Tokens accept a literal string, `{env: NAME}`, or `{file: PATH}`. Relative secret
paths resolve against the config file's canonical directory (the target directory
for symlinks); relative file arguments resolve against the working directory.
Unknown and duplicate fields are rejected. Loading errors identify the selected
file without printing secret values. See the [configuration guide](../../docs/configuration.md).

Catch `ValidationError`, `TransportError` (with `timed_out`), `HTTPError` (with `status_code`), or `DecodeError`; all inherit `FlaresError`. Error messages do not include secrets or provider response bodies. A successful HTTP response with notification status `failed` is a normal result, not an exception. `pending` means accepted, not delivered. Requests never retry or follow redirects automatically. Reuse an explicit alert idempotency key after an uncertain outcome.

Development (from this directory): `uv sync --locked`, `uv run pytest tests`, `uv build`. Run the repository contract suite after building the server and Rust contract driver; see [Development](../../docs/development.md).
