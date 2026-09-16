# Flare

A Rust HTTP service and command-line client for tracking issues by ID, with SQLite persistence, Pushover and webhook notifications, and heartbeat monitoring. One `flare` binary provides both the server and client; no Python runtime is needed.

## Build and run

Install a current stable Rust toolchain, then:

```sh
cargo build --release --locked
cp examples/server.yaml server.yaml
cp examples/client.yaml client.yaml
```

Edit the YAML files before starting. Set a shared API token in both files. To enable notifications, add your Pushover application token and user/group key under `pushover` in `server.yaml`. Register an application and obtain credentials through [Pushover](https://pushover.net/api). Omit `pushover` to run without notifications.

```sh
./target/release/flare serve --config server.yaml
```

In another terminal:

```sh
./target/release/flare --config client.yaml open disk-space \
  --title 'Disk space low' --message 'Less than 5% free on the backup server.'
./target/release/flare get disk-space
./target/release/flare list --status open
./target/release/flare close disk-space
./target/release/flare --json open disk-space
```

Configuration defaults to `server.yaml` for `serve` and `client.yaml` for other commands. Global `--config` and `--json` options work before or after the command. `cargo run --locked -- …` also works during development. To install the binary locally, run `cargo install --path . --locked`.

## Configuration

Server YAML:

```yaml
host: 127.0.0.1                    # IP address; default is loopback
port: 8000
database: flare.sqlite3          # relative to the YAML file's directory
api_token: replace-with-a-long-random-shared-token
pushover:                        # optional; omit this whole section to disable notifications
  app_token: replace-with-your-30-char-token
  user_key: replace-with-your-30-char-key
  # device: phone                # optional; otherwise all recipient devices
```

The `api_token` is required. The `pushover` section is optional; omitting it or setting it to `null` disables notifications. If supplied, both Pushover keys are required and must each contain 30 ASCII letters/digits; `user_key` also accepts a group key. Optional `device` is one device name, up to 25 letters, digits, underscores, or hyphens. The server creates the database and its parent directory. Unknown settings, incomplete Pushover sections, and invalid settings fail startup. Parser errors and provider response bodies are not logged because they may contain secrets.

Client YAML only needs the service credentials:

```yaml
base_url: http://127.0.0.1:8000
api_token: replace-with-a-long-random-shared-token
timeout: 15                       # seconds; total request timeout
```

Keep real configuration files private; `server.yaml` and `client.yaml` are gitignored. For remote use, terminate HTTPS at a reverse proxy and point clients to its HTTPS URL. The built-in listener serves HTTP and defaults to localhost. Run one service instance per database; SQLite is bundled into the binary. SIGINT and SIGTERM stop the listener gracefully.

## HTTP API

All `/v1` operations and `/metrics` require `Authorization: Bearer <api_token>`. Bodies and responses are JSON. The generated OpenAPI document is available at `/openapi.json` without authentication; it contains schemas and the bearer security definition, not configuration or issue data.

| Method | Endpoint | Input / behavior |
| --- | --- | --- |
| POST | `/v1/alerts` | `{"title":"Backup complete","message":"All files copied."}`; sends a one-shot notification. |
| POST | `/v1/issues/open` | `{"id":"disk-space","title":"Optional title","message":"Optional message"}` |
| POST | `/v1/issues/close` | `{"id":"disk-space"}` |
| GET | `/v1/issues/{id}` | Fetch an issue by URL-encoded ID. |
| GET | `/v1/issue?id=…` | Equivalent lookup using query encoding; the CLI uses this to preserve arbitrary IDs. |
| GET | `/v1/issues` | List; accepts `status=open` or `closed`, `limit` (1–1000; default 100), and `offset` (default 0). |

IDs are case-sensitive strings of 1–200 Unicode characters. They are stored verbatim. Use the query lookup for IDs containing dot path segments such as `.` or `a/../b`, which URL libraries/proxies may normalize in a path. Titles must contain 1–250 characters and messages 1–1024 characters, matching [Pushover's limits](https://pushover.net/api#limits). Fields are optional or nullable; empty strings are rejected.

Opening a new issue sets its state to `open`. Closing sets its state to `closed`. Opening a closed issue reopens it and increments its `opening_count`. Every actual opening uses that call's content, defaulting the title to the ID and the message to `Issue {id} opened.` or `Issue {id} reopened.` An already-open call does not update content, timestamps, or notifications. An already-closed call also does nothing. Closing or fetching an unknown issue ID returns 404.

Successful mutations, including no-ops and saved openings with notification failures, return HTTP 200:

```json
{
  "delivery_id": 42,
  "issue": {
    "severity": "warning",
    "remind_every_seconds": null,
    "notify_on_resolution": false,
    "delivery_id": 42,
    "id": "disk-space",
    "status": "open",
    "title": "disk-space",
    "message": "Issue disk-space opened.",
    "created_at": "2026-09-14T12:00:00Z",
    "updated_at": "2026-09-14T12:00:00Z",
    "opened_at": "2026-09-14T12:00:00Z",
    "closed_at": null,
    "opening_count": 1,
    "notification": {"status": "sent", "error": null}
  },
  "changed": true,
  "notification": {"status": "sent", "error": null}
}
```

`issue.notification` is the latest opening's recorded outcome. The top-level `notification` describes the attempt made by this request, so duplicate-open responses use `not_attempted`, as do closes without a resolution notification. Reads return the issue object directly. Lists return `{"items":[…],"total":1,"limit":100,"offset":0}`, ordered by `updated_at` descending and ID ascending for ties. Timestamps use UTC. `closed_at` retains the latest closing time after reopening. Mutation responses describe the transition's snapshot; a concurrent request may subsequently change the issue.

Errors use `{"detail":"…"}`: 401 for authentication failures, 404 for missing issues, 422 for invalid field values/types, and 503 for unavailable storage. Invalid JSON syntax returns 400, non-JSON content types 415, and bodies over 32 KiB 413. Storage/transport failures can occur after a transition was committed; inspect the issue when an outcome is uncertain.

For example, with `FLARE_API_TOKEN` set to the shared token in your shell:

```sh
curl --fail-with-body http://127.0.0.1:8000/v1/issues/open \
  -H "Authorization: Bearer $FLARE_API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"id":"disk-space","title":"Disk space low"}'

curl --fail-with-body http://127.0.0.1:8000/v1/issues/close \
  -H "Authorization: Bearer $FLARE_API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"id":"disk-space"}'
```

## One-shot alerts

```sh
flare alert --title 'Backup complete' --message 'All files copied.' \
  --severity info --idempotency-key backup-2026-09-15
```

Equivalent HTTP request:

```sh
curl --fail-with-body http://127.0.0.1:8000/v1/alerts \
  -H "Authorization: Bearer $FLARE_API_TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: backup-2026-09-15' \
  -d '{"title":"Backup complete","message":"All files copied.","severity":"info"}'
```

Both `title` (1–250 Unicode characters) and `message` (1–1024) are required. Empty/null values and unknown fields are rejected. Alerts have no issue ID or close operation. Their delivery history is stored separately from issues.

The HTTP 200 response contains `delivery_id` and `notification`, for example:

```json
{"delivery_id":42,"notification":{"status":"sent","error":null}}
```

`sent` means every selected destination accepted the notification. `pending` means it is queued, being sent, grouped, rate limited, or awaiting a retry. `failed` means the attempt limit was reached for at least one destination; `error` gives a sanitized explanation. `not_attempted` means the route has no destinations. Inspect individual outcomes with `flare delivery 42` or authenticated `GET /v1/deliveries/42`.

`Idempotency-Key` is optional and accepts 1–200 printable ASCII characters without spaces. Reusing a key with the same parsed request returns the same delivery ID and its current outcome, including after restart; reusing it with different content returns HTTP 409. Keys are retained indefinitely in this version. Without a key, each request creates a new delivery. Reuse the same key after a timeout or lost response.

### Grouping and rate limits

Supply `group_key` in an alert body, or `--group-key backups`, to group alerts for `delivery.group_window_seconds` (default 30). Alerts with the same key, severity, and destinations share one delivery during the window. The window is fixed from the first alert and persisted separately from the next delivery attempt: rate limiting, retries, restarts, and configuration changes do not extend it. The notification contains the count and latest title/message, truncated to the provider's message limit. Idempotent retries do not increase the count. Set the window to 0 to disable grouping. On upgrade, existing deliveries without a recorded grouping deadline remain queued but no longer accept additional alerts.

`delivery.rate_limit_per_minute` limits destination attempts across all alert types, including retries (default 120; 0 disables the limit). It uses fixed UTC minute buckets. Excess work stays queued without spending an attempt. Up to four deliveries run concurrently, with at most two requests per Pushover destination. The service reserves destination capacity before spending an attempt and starting the request deadline; waiting for capacity does not consume either.

## Delivery configuration and routing

Existing top-level `pushover` configuration remains supported and supplies the default destination. Named destinations allow separate routing by severity:

```yaml
delivery:
  max_attempts: 3                # default 1; includes the initial attempt
  retry_base_seconds: 10         # default 10
  retry_max_seconds: 3600        # default 3600
  rate_limit_per_minute: 120
  group_window_seconds: 30

destinations:
  audit:
    type: webhook
    url: https://hooks.example.com/flare
    bearer_token: replace-with-webhook-token  # optional
  urgent:
    type: pushover
    config:
      app_token: replace-with-your-30-char-token
      user_key: replace-with-your-30-char-key
      device: phone

default_destinations: [audit]
routes:
  critical: [audit, urgent]
  info: [audit]
```

Severity is `info`, `warning` (default), or `critical`. A route replaces the default destination list for that severity; an empty list explicitly disables it. Named destinations are used only when listed in defaults or a route. Pushover priorities are respectively -1, 0, and 1; critical alerts do not use Pushover's repeating emergency mode.

Webhooks receive JSON containing `id` (delivery ID), `title`, `message`, `severity`, `kind`, and `count`. Optional bearer authentication and a stable `Idempotency-Key: flare-delivery-<id>` header accompany each attempt. The receiving service can deduplicate retries using that key; use separate key scopes for separate Flare databases. Any HTTP 2xx response succeeds. Redirects are not followed, provider response bodies are not exposed, and HTTP requests have a ten-second deadline. Configure HTTPS for remote webhook destinations. URLs and credentials remain in server configuration and are excluded from delivery records and logs.

## Issue reminders and resolution notifications

```sh
flare open disk-space --title 'Disk space low' --message 'Less than 5% free.' \
  --severity critical --remind-every-seconds 3600 --notify-on-resolution
```

The corresponding optional fields on `POST /v1/issues/open` are `severity`, `remind_every_seconds`, and `notify_on_resolution`. Reminder intervals accept 1–31536000 seconds. Reminders are disabled when the interval is omitted; resolution notifications default to false. An already-open request remains a no-op, including its settings. Reopening replaces the settings with those on the new request. `GET` and list responses expose the current settings and opening delivery ID.

The scheduler creates reminders while the issue remains open, at most one per check; missed intervals during downtime are not replayed individually. Closing stops future reminders and optionally creates one resolution delivery. Repeated closes do not notify. Already queued deliveries remain eligible for delivery. Reminder and resolution outcomes are separate from the issue's latest opening outcome.

## Heartbeat monitoring

Register an expected check-in, then call `beat` after each successful run:

```sh
flare heartbeat add nightly-backup --title 'Nightly backup' \
  --interval-seconds 86400 --grace-seconds 3600 --severity critical --notify-on-recovery
flare heartbeat beat nightly-backup
flare heartbeat list
flare heartbeat remove nightly-backup
```

| Method | Endpoint | Behaviour |
| --- | --- | --- |
| POST | `/v1/heartbeats` | Create or replace a monitor with `id`, `title`, `interval_seconds`, optional `grace_seconds`, `severity`, and `notify_on_recovery`. |
| POST | `/v1/heartbeats/check-in` | Reset the deadline using `{"id":"nightly-backup"}`. Unknown IDs return 404. |
| GET | `/v1/heartbeats` | List monitors, including `last_seen`, `due_at` (Unix seconds), and `overdue`. |
| DELETE | `/v1/heartbeat?id=nightly-backup` | Remove a monitor; query encoding supports arbitrary IDs. |

Registration starts the deadline immediately and replacing a monitor resets its state. The deadline is the last check-in plus the interval and grace period. Intervals accept 1–31536000 seconds; grace accepts 0–31536000. The scheduler checks once per second and creates one missed-heartbeat delivery per outage. A later check-in resets the deadline and optionally queues a recovery notification. Monitor state, deadlines, and queued notifications survive restart. Removing a monitor stops future checks; existing queued notifications remain eligible for delivery.

## Health and delivery visibility

- `GET /healthz`: unauthenticated liveness; HTTP 200 while the HTTP service responds.
- `GET /readyz`: unauthenticated database readiness; HTTP 200 if a database query succeeds, otherwise 503. It does not probe external providers.
- `GET /metrics`: requires the shared bearer token; Prometheus text counters for destination attempts, sent/failed attempts, skipped deliveries, accumulated delivery latency, and queue depth. Counters survive restart.
- `GET /v1/deliveries/{id}`: authenticated delivery details, destination outcomes, attempt counts, and next attempt time (Unix seconds).

Structured tracing events record delivery ID, destination name, attempt number, outcome, and latency. Notification text and credentials are not logged. The generated OpenAPI document includes the API operations and schemas.

## Durable delivery semantics

Issue transitions and their delivery jobs commit in one SQLite transaction. Alert jobs and idempotency keys also commit together. The service tries immediately when work is due and capacity permits, and its background worker handles grouping, rate-limited work, retries, reminders, and heartbeat notifications.

`max_attempts` defaults to 1 to preserve the previous one-attempt policy; set it above 1 to enable retries. Failed destinations retry with exponential delays capped by `retry_max_seconds`. Destinations with recorded success are not sent again. Both queued jobs and incomplete jobs recover on startup, subject to the attempt limit. Requests may return `pending`; CLI exit 0 in that case means accepted, not delivered. Inspect the delivery later for its final outcome.

Delivery is best effort with bounded retries, not exactly once. A provider may accept a message before a timeout or a crash prevents recording success, so a retry can duplicate it. A crash after reserving an attempt consumes that attempt even if the request was not sent. With no attempts left, the job fails rather than retrying indefinitely. Old database entries that predate durable jobs retain the earlier `unknown` outcome for interrupted notifications and cannot be replayed.

Run one service instance per database. Delivery records and idempotency keys are retained indefinitely; no automatic cleanup runs. A delivery uses the destination names selected when it was created and the current configuration for those names when sent. Removing a configured destination makes its queued attempts fail. Increasing retry limits affects queued jobs; completed failures are not automatically reopened. Disabled routes record `not_attempted` and enabling them later does not replay skipped notifications.

CLI exit codes:

| Code | Meaning |
| --- | --- |
| 0 | Successful command, accepted/queued delivery, or no-op. |
| 1 | Argument, configuration, HTTP, or transport error. |
| 2 | Alert or issue transition saved, but delivery failed. |

`--json` returns the API result unchanged for successful calls and notification failures. Application errors produce `{"error":"…"}`; command-line parsing errors use the standard help/error text. A duplicate open succeeds with exit 0 even if the issue's previous notification failed. Use `get` to inspect historical outcomes.

## Client libraries

All clients live in this repository and cover issues, alerts, delivery inspection,
heartbeats, health, readiness, and metrics:

| Client | Package | Documentation |
| --- | --- | --- |
| Rust (async) | `flare-client`, with shared `flare-types` | [Rust client](clients/rust/README.md) |
| Python (sync and async) | `flare-client`, imported as `flare_client` | [Python client](clients/python/README.md) |
| Bash | Sourceable `flare.sh`, using curl and jq | [Bash client](clients/bash/README.md) |

The root Cargo package remains the server and CLI. `crates/flare-types` contains the
wire models, with opt-in OpenAPI and CLI derives. `clients/rust` has no server or
database dependencies. Python packaging and development use uv. Clients take their
URL, bearer token, and timeout directly; server and CLI YAML configuration is unchanged.

The [OpenAPI contract](api/openapi.json) is generated from the server. Client methods
are handwritten and checked against shared scenarios under `tests/contract`.
Clients do not automatically retry requests or follow redirects. Inspect notification
status even after HTTP success; Bash uses exit 2 for an issue/alert mutation whose
notification failed. Reuse an alert's idempotency key after an uncertain outcome.

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --examples --locked
cargo build --bin flare --locked
uv sync --project clients/python --locked
uv run --project clients/python ruff check --config clients/python/pyproject.toml clients/python tests/contract scripts
uv run --project clients/python ruff format --check --config clients/python/pyproject.toml clients/python tests/contract scripts
bash clients/bash/tests/source.sh
uv run --project clients/python pytest -q
```

Commit `Cargo.lock` for reproducible dependency resolution. Tests use temporary databases and fake notification channels/local HTTP servers; they do not contact Pushover or send real alerts. Coverage includes lifecycle transitions, concurrent calls, late outcomes, restarts, YAML validation, authentication, CLI behavior, and provider failures.

Commit `clients/python/uv.lock` as well. The shared suite requires Bash, curl, jq,
and the debug server and Rust adapter built above. Python's unit tests can run
independently with `uv run --project clients/python pytest clients/python/tests`.
CI also runs ShellCheck, detects OpenAPI/version drift, and builds package artifacts.

## Releases

The server and all clients share one version and `vX.Y.Z` tag. Update the workspace
version, local Cargo dependency versions, Python package and `__version__`, Bash
`FLARE_CLIENT_VERSION`, both lockfiles, and the changelog together. Regenerate
`api/openapi.json` using the command in [api/README.md](api/README.md).

```sh
uv run --project clients/python python scripts/check_versions.py
uv run --project clients/python python scripts/build_artifacts.py
```

The build script creates a native server/CLI archive, two Rust `.crate` files,
a Python wheel and source distribution, and a Bash archive in `dist/`. It verifies
the extracted Rust packages without server dependencies and imports the installed
Python wheel in isolation. CI uploads these artifacts on Linux; nothing is published
automatically. Publish `flare-types` before `flare-client` when releasing to crates.io;
Python and Bash have separate artifacts from the same tag. Record release changes
in [CHANGELOG.md](CHANGELOG.md).
