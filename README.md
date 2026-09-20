# Flares

Track recurring issues, monitor heartbeats, and send notifications through Pushover or webhooks. Opening or reopening an issue triggers a notification; opening an already-open issue is a no-op. State is stored in SQLite.

## Install

The same binary provides the server and command-line client:

```sh
cargo install flares --locked
```

Alternatively, download a binary for your platform from [GitHub Releases](https://github.com/torpedro/flares/releases), extract it, and put `flares` on your `PATH`. To install from a source checkout, run `cargo install --path . --locked`.

## Run the server

Create `~/.config/flares` with `mkdir -p ~/.config/flares`, then save this as `~/.config/flares/server.yaml`:

```yaml
server:
  listen: 127.0.0.1:8000
  api_token: {env: FLARES_API_TOKEN}
storage:
  database: flares.sqlite3
```

Set a shared token, validate the configuration, and start the server:

```sh
export FLARES_API_TOKEN='replace-with-a-long-random-token'
flares config check
flares serve
```

To enable Pushover, add these sections to your server config and set the two credential environment variables before starting:

```yaml
destinations:
  phone:
    type: pushover
    app_token: {env: PUSHOVER_APP_TOKEN}
    user_key: {env: PUSHOVER_USER_KEY}
routing:
  default: [phone]
```

Notifications are disabled until destinations are configured and selected in routing. Database paths resolve relative to the YAML file. Run one server per database; use an HTTPS reverse proxy for remote access.

See the [configuration guide](docs/configuration.md) for webhooks, routing, retries, secret files, and retention, or start with the [complete example](examples/server.yaml).

## Use the CLI

Save this as `~/.config/flares/client.yaml` and set `FLARES_API_TOKEN` to the same token in your client shell:

```yaml
base_url: http://127.0.0.1:8000
api_token: {env: FLARES_API_TOKEN}
timeout: 15s
```

```sh
flares open disk-space --title 'Disk space low' --message 'Less than 5% free.'
flares get disk-space
flares list --status open
flares close disk-space

flares alert --title 'Backup complete' --message 'All files copied.' \
  --severity info --idempotency-key backup-run-123

flares heartbeat add nightly-backup --title 'Nightly backup' --interval-seconds 86400
flares heartbeat beat nightly-backup
```

Without `--config`, Flares looks in `$XDG_CONFIG_HOME/flares` (when absolute), otherwise `~/.config/flares`, then `/etc/flares`, using `server.yaml` for server/config commands and `client.yaml` for client commands (including `config --client`). The current directory is not searched. Use `--config /path/to/file.yaml` to override lookup. Add `--json` for structured output or `--help` for all commands. YAML durations require units such as `15s`.

Exit codes: **0** for success, no-op, or queued delivery; **1** for errors; **2** when a mutation was saved but notification delivery failed. Inspect queued deliveries with `flares delivery ID`.

## Client libraries

Libraries take their server URL and token directly and cover issues, alerts, deliveries, and heartbeats.

### Python

Requires Python 3.11+:

```sh
uv add flares-client
```

```python
import os
from flares_client import Client

with Client("http://127.0.0.1:8000", os.environ["FLARES_API_TOKEN"]) as client:
    result = client.open_issue("backup", title="Backup failed")
    print(result.notification.status)
    client.close_issue("backup")
```

[Python documentation](clients/python/README.md) includes the asynchronous `AsyncClient`.

### Rust

```sh
cargo add flares-client
```

Inside your Tokio async function:

```rust
use flares_client::{ApiClient, OpenIssue};

let client = ApiClient::new("http://127.0.0.1:8000", std::env::var("FLARES_API_TOKEN")?)?;
let result = client.open(OpenIssue { id: "backup".into(), ..Default::default() }).await?;
client.close("backup".into()).await?;
```

See the [Rust documentation](clients/rust/README.md) for types, errors, and all methods.

### Bash

Download and extract the Bash archive from [GitHub Releases](https://github.com/torpedro/flares/releases), then source its `flares.sh`. Requires Bash, curl, and jq. From a source checkout:

```bash
source clients/bash/flares.sh
export FLARES_BASE_URL=http://127.0.0.1:8000
export FLARES_API_TOKEN='your-server-token'

flares_open_issue backup --title "Backup failed" --severity critical
flares_close_issue backup
flares_alert "Backup" "Finished" --idempotency-key backup-run-123
```

See the [Bash documentation](clients/bash/README.md) for options and exit-code handling.

## HTTP API and further documentation

The HTTP API uses JSON and bearer authentication. OpenAPI is served at `/openapi.json`; `/healthz` and `/readyz` provide health checks.

- [API and delivery behavior](docs/api.md) — endpoints, curl examples, notification outcomes, and retries.
- [Configuration](docs/configuration.md) — server and CLI settings, including upgrades.
- [Development](docs/development.md) — building and running tests.
- [Manual releases](docs/releases.md) and [changelog](CHANGELOG.md).
