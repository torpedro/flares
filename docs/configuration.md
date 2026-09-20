# Configuration

The server and CLI read YAML files. Rust and Python SDKs can explicitly load the same client YAML, or accept URL, token, and timeout arguments directly. Start from [server.yaml](../examples/server.yaml) and [client.yaml](../examples/client.yaml).

## Configuration lookup

An explicit `--config PATH` always selects that file, including relative paths.
Otherwise, Flares checks these locations in order:

1. `$XDG_CONFIG_HOME/flares/server.yaml` or `client.yaml`, when `XDG_CONFIG_HOME` is an absolute path; otherwise `$HOME/.config/flares/server.yaml` or `client.yaml`.
2. `/etc/flares/server.yaml` or `/etc/flares/client.yaml`.

An unset, empty, or relative `XDG_CONFIG_HOME` falls back to `$HOME/.config`.
When an absolute `XDG_CONFIG_HOME` is set, it replaces `$HOME/.config`; the latter
is not searched as an additional location. If neither a usable XDG directory nor
a nonempty `HOME` is available, only `/etc/flares` is checked.

`serve` and server `config check/show` use `server.yaml`. Client commands and
`config check/show --client` use `client.yaml`. The current directory is never
searched automatically. One file is selected, without merging. Lookup falls back
only when a candidate is absent; invalid or unreadable files, directories, and
broken symlinks produce an error rather than silently selecting another config.
Missing-file errors list the locations searched. Explicit paths never fall back.

Relative `--config` and SDK file arguments resolve against the working directory.
Relative paths **inside** YAML (database and secret files) resolve against the
selected file's canonical directory. For a symlinked config, this is the target's
directory. Absolute paths remain absolute. There is no automatic environment
variable override of settings; `{env: NAME}` is an explicit secret reference.

Create the user directory with `mkdir -p ~/.config/flares`. For system installations,
place configuration in `/etc/flares` and explicitly set `storage.database` to a
writable data location, such as `/var/lib/flares/flares.sqlite3`.

## Validate and inspect

```sh
flares config check --config server.yaml
flares config show --config server.yaml
flares config show --config server.yaml --json
flares config check --client --config client.yaml
flares config show --client --config client.yaml
```

`check` validates types, values, secret references, and routing. It reads the config and referenced secret files/environment variables; it does not create directories, open a database, bind a port, or contact destinations. It returns exit 0 for success or 1 for an error. `--json` produces machine-readable output. Server config is the default; use `--client` to select the client schema and default filename.

`show` performs the same validation and prints effective settings, including defaults, resolved database paths, and inherited destination retry policies. Credentials and webhook URLs are always `[REDACTED]`. This is diagnostic output, not a deployable config containing credentials.

Loading errors identify the selected configuration file. Validation errors include a field path and a YAML line/column for parsing errors when available. Semantic errors identify the field and constraint, for example `delivery.retry.max_delay: must be at least base_delay`. Parser source excerpts and secret values are never printed. Unknown fields and legacy aliases are rejected.

## SDK file loading

The CLI, Rust SDK, and Python sync/async SDKs accept this same client format:

```yaml
base_url: http://127.0.0.1:8000
api_token: {file: secrets/api-token}
timeout: 15s
```

`api_token` is required and accepts a literal string, `{env: NAME}`, or `{file: PATH}`.
Omitting `base_url` or `timeout` uses the defaults shown above. Unknown fields and
duplicate fields are rejected. Loading reads files and referenced environment
variables but makes no API requests.

```rust,ignore
let client = flares_client::ApiClient::from_config("/path/client.yaml")?;
// Explicitly opt into the same discovery rules as the CLI:
let client = flares_client::ApiClient::from_default_config()?;
```

```python
from flares_client import Client, AsyncClient

with Client.from_config("/path/client.yaml") as client:
    result = client.health()

async def health():
    async with AsyncClient.from_default_config() as client:
        return await client.health()
```

Both Python classes provide both constructors. `AsyncClient.from_config()` and
`from_default_config()` are synchronous constructors; network operations remain
async. Direct URL/token constructors never load config files or discover settings.
Rust reports loading errors as `Error::Configuration`; Python uses `ValidationError`.

The Bash library continues to accept shell variables. For YAML-based shell scripts,
use the CLI, for example `flares --config /path/client.yaml alert --title Backup --message Done`.

## Server format

```yaml
server:
  listen: 127.0.0.1:8000
  api_token: {env: FLARES_API_TOKEN}

storage:
  database: flares.sqlite3
  retention:
    deliveries: 30d
    idempotency_keys: 90d

delivery:
  retry:
    max_attempts: 3
    base_delay: 10s
    max_delay: 1h
  rate_limit:
    attempts: 120
    window: 1m
  group_window: 30s
  queue_limit: 10000

destinations:
  phone:
    type: pushover
    app_token: {env: PUSHOVER_APP_TOKEN}
    user_key: {file: /run/secrets/pushover-user-key}
  audit:
    type: webhook
    url: https://hooks.example.com/flares
    bearer_token: {env: WEBHOOK_TOKEN}
    delivery:
      retry:
        max_attempts: 5
        base_delay: 30s
      rate_limit:
        attempts: 20
        window: 1m

routing:
  default: [audit]
  severity:
    critical: [audit, phone]
    info: []
```

The example above opts into retries and retention. The distributed example file explicitly shows the actual defaults, including one attempt and indefinite retention.

`server.listen` is an IP address and port; IPv6 uses brackets, such as `'[::1]:8000'`. Its default is `127.0.0.1:8000`. The token is required. Database paths default to `flares.sqlite3` and resolve relative to the config file, never the working directory.

### Secrets

Each secret accepts exactly one of these forms:

```yaml
api_token: literal-value
api_token: {env: FLARES_API_TOKEN}
api_token: {file: /run/secrets/flares-api-token}
```

This applies to server/client API tokens, Pushover credentials, webhook bearer tokens, and webhook URLs. References resolve once at startup or config checking; missing variables/files fail validation. File references resolve relative to the config file and strip trailing CR/LF characters, allowing files written by ordinary secret-management tools. Other whitespace is preserved and validated normally. There is no `${...}` interpolation in arbitrary YAML strings.

### Durations and defaults

Durations accept a nonnegative integer followed by `s`, `m`, `h`, `d`, or `w`, such as `10s`, `30m`, or `1h`. Duration values must be strings with a unit, including client `timeout`. Bare numbers and fractional values are rejected; durations have whole-second precision.

| Setting | Default | Constraint |
| --- | --- | --- |
| `delivery.retry.max_attempts` | `1` | 1–20, including the initial attempt |
| `delivery.retry.base_delay` | `10s` | 1s–1d |
| `delivery.retry.max_delay` | `1h` | At least base delay, at most 1d |
| `delivery.rate_limit.attempts` | `120` | Destination attempts per window; 0 disables |
| `delivery.rate_limit.window` | `1m` | 1s–1d |
| `delivery.group_window` | `30s` | 0s–1d; applies only when an alert supplies a group key |
| `delivery.queue_limit` | `10000` | Queued/running delivery jobs; 0 disables |
| `storage.retention.deliveries` | `null` | No deletion; or 1s–3650d after completion |
| `storage.retention.idempotency_keys` | `null` | No expiration; or 1s–3650d after key creation |
| Client `timeout` | `15s` | Greater than 0, at most 1d |

### Destinations and routing

Define all destinations under `destinations`. Provider fields sit directly beneath `type`. A Pushover destination requires `app_token` and `user_key`, with optional `device`; a webhook requires `url`, with optional `bearer_token`. Names contain 1–64 ASCII letters, digits, underscores, or hyphens.

`routing.default` selects destinations when no severity-specific route exists. A severity route replaces that default list. An explicit empty list always means “send nowhere.” Named destinations alone do not enable notifications. Unknown names and duplicate names within a route are errors.

### Destination overrides

A destination may override individual retry fields under `delivery.retry`. Omitted fields inherit the global values. Each destination has its own attempt count and retry deadline; a long backoff for one destination does not delay another destination's eligible retry.

A destination's `delivery.rate_limit` is an additional limit, not a replacement for the global service budget. It requires both `attempts` and `window`. Fixed windows are aligned to Unix time, and counters survive restart. Work waiting for either limit spends no attempt. A rate-limited destination does not prevent other selected destinations from being attempted. Setting its attempts to 0 disables only the destination limit.

The existing four-delivery concurrency cap and provider-specific capacity limits still apply. Changes require restarting the service. Queued jobs use current policies and credentials for their stored destination names; completed jobs are not reopened.

### Queue limits and retention

When the queue is full, new work that requires a delivery receives HTTP 503 with `Delivery queue is full; retry later`. Issue transitions and heartbeat recoveries that need a delivery roll back atomically, so they can be retried. Existing idempotency keys and alerts joining an existing group still work. Destinations-disabled operations do not occupy the active queue. Scheduled checks retain their progress and retry unscheduled work after capacity becomes available.

Cleanup runs at startup and approximately once per minute. It deletes only completed delivery records, never queued/running jobs. Delivery age is measured from completion; key age is measured from creation. An unexpired idempotency key pins its delivery record even when the delivery-retention period has elapsed. Keys belonging to active deliveries do not expire until delivery finishes. Deleting delivery records preserves issue lifecycle and notification history; an issue's opening delivery ID becomes null once that record is removed.

After a key expires, reusing it can create a new notification. Choose a key-retention period longer than your callers' retry window. With indefinite key retention, keyed delivery records remain indefinitely regardless of the delivery-retention setting. Retention applies to delivery history, not issue or heartbeat records; the active queue limit is not a database file-size cap. SQLite can reuse freed pages without immediately shrinking its file.

## Updating old configuration files

Only the structured format is accepted. Legacy fields are rejected, even when combined with valid modern fields. Convert old files before starting the service:

| Legacy | Preferred form |
| --- | --- |
| `host` and `port` | `server.listen` |
| `api_token` | `server.api_token` |
| `database` | `storage.database` |
| Top-level `pushover` | `destinations.pushover` with `type: pushover` |
| `destinations.NAME.config` for Pushover | Provider fields directly inside `destinations.NAME` |
| `default_destinations` | `routing.default` |
| `routes` | `routing.severity` |
| `delivery.max_attempts` | `delivery.retry.max_attempts` |
| `delivery.retry_base_seconds` | `delivery.retry.base_delay` |
| `delivery.retry_max_seconds` | `delivery.retry.max_delay` |
| `delivery.rate_limit_per_minute` | `delivery.rate_limit: {attempts: N, window: 1m}` |
| `delivery.group_window_seconds` | `delivery.group_window` |

Set `routing.default` explicitly to enable notifications; omitted or empty defaults send nowhere. Pushover credentials must be directly beneath `type: pushover`, without a `config` wrapper. Convert numeric durations to strings such as `30s`; this also applies to client `timeout`. Run `flares config check` (or `--client`) after editing.

Existing databases migrate automatically. Legacy records begin their retention age at migration so enabling cleanup does not immediately discard old deliveries or keys. Existing per-destination schedules, grouping windows, and idempotency mappings survive subsequent restarts. If you migrate top-level Pushover to a named destination, keep its name `pushover` so already queued jobs continue to resolve it.

## Upgrading from Flare

The executable is now `flares`. Python imports use `flares_client`; Bash users source
`flares.sh`, call `flares_*` functions, and set `FLARES_*` variables. Update secret
environment references in your YAML if you rename those variables. Metrics now use
the `flares_` prefix. For an existing installation, explicitly set `storage.database`
to the existing database (for example, `flare.sqlite3`) before starting: the new
default is `flares.sqlite3`. Webhook idempotency keys keep their original
`flare-delivery-` prefix to preserve deduplication across upgrades.
