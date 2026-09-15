# Flare

A Rust HTTP service and command-line client for tracking issues by ID, with SQLite persistence and Pushover notifications. One `flare` binary provides both the server and client; no Python runtime is needed.

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

All `/v1` operations require `Authorization: Bearer <api_token>`. Bodies and responses are JSON. The generated OpenAPI document is available at `/openapi.json` without authentication; it contains schemas and the bearer security definition, not configuration or issue data.

| Method | Endpoint | Input / behavior |
| --- | --- | --- |
| POST | `/v1/alerts` | `{"title":"Backup complete","message":"All files copied."}`; sends a one-shot notification. |
| POST | `/v1/issues/open` | `{"id":"disk-space","title":"Optional title","message":"Optional message"}` |
| POST | `/v1/issues/close` | `{"id":"disk-space"}` |
| GET | `/v1/issues/{id}` | Fetch an issue by URL-encoded ID. |
| GET | `/v1/issue?id=…` | Equivalent lookup using query encoding; the CLI uses this to preserve arbitrary IDs. |
| GET | `/v1/issues` | List; accepts `status=open` or `closed`, `limit` (1–1000; default 100), and `offset` (default 0). |

IDs are case-sensitive strings of 1–200 Unicode characters. They are stored verbatim. Use the query lookup for IDs containing dot path segments such as `.` or `a/../b`, which URL libraries/proxies may normalize in a path. Titles must contain 1–250 characters and messages 1–1024 characters, matching [Pushover's limits](https://pushover.net/api#limits). Fields are optional or nullable; empty strings are rejected.

Opening a new issue sets its state to `open`. Closing sets its state to `closed`. Opening a closed issue reopens it and increments its `opening_count`. Every actual opening uses that call's content, defaulting the title to the ID and the message to `Issue {id} opened.` or `Issue {id} reopened.` An already-open call does not update content, timestamps, or notifications. An already-closed call also does nothing. Closing or fetching an unknown ID returns 404.

Successful mutations, including no-ops and saved openings with notification failures, return HTTP 200:

```json
{
  "issue": {
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

`issue.notification` is the latest opening's recorded outcome. The top-level `notification` describes the attempt made by this request, so close and duplicate-open responses use `not_attempted`. Reads return the issue object directly. Lists return `{"items":[…],"total":1,"limit":100,"offset":0}`, ordered by `updated_at` descending and ID ascending for ties. Timestamps use UTC. `closed_at` retains the latest closing time after reopening. Mutation responses describe the transition's snapshot; a concurrent request may subsequently change the issue.

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

Send `POST /v1/alerts` to trigger a notification without creating or updating an issue. Both `title` (1–250 Unicode characters) and `message` (1–1024) are required. Empty/null values and unknown fields are rejected. Alerts have no ID, stored history, or close operation.

```sh
curl --fail-with-body http://127.0.0.1:8000/v1/alerts \
  -H "Authorization: Bearer $FLARE_API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"title":"Backup complete","message":"All files copied."}'
```

The response is HTTP 200 with `{"notification":{"status":"sent","error":null}}` when Pushover accepts the notification. A delivery failure returns HTTP 200 with `status: "failed"` and a sanitized `error`; disabled notifications return `status: "not_attempted"` and `error: null`.

Each valid request makes one attempt when notifications are configured, including repeated identical requests. The existing Pushover timeout and concurrency limits apply, with no automatic retries. Once started, the attempt continues if the client disconnects while the server process remains running. A timeout or lost response can leave delivery uncertain; retrying can send another notification. Alerts are not persisted or replayed after restart.

## Notification semantics

With Pushover configured, each actual opening atomically stores both the issue transition and a `pending` outcome before contacting Pushover. Concurrent duplicate opens cannot produce another attempt. Delivery uses normal priority, verified HTTPS, a ten-second total deadline (including waiting for one of two connection slots), no redirects, and no retries. `sent` means Pushover accepted the message, not that a device displayed it.

Without Pushover, opening and reopening still succeed, with `not_attempted` recorded atomically as the notification outcome and CLI exit code 0. This outcome survives restarts. Existing databases are upgraded automatically, preserving notification history. Enabling Pushover later does not send alerts for already-open issues; subsequent new openings and reopenings use the configured channel.

If delivery fails, the issue stays open and the response contains `notification.status=failed` with a sanitized explanation. Timeouts may mean delivery occurred without an acknowledgement. Repeating `open` will not resend. Closing never sends notifications. Outcomes are keyed by opening count so a delayed result from an earlier opening cannot overwrite the latest outcome.

On restart, unfinished `pending` outcomes become `unknown` and are never retried. A crash between committing an opening and sending its notification can lose that notification; a crash after sending can leave its outcome unknown. This is deliberately best-effort, one-attempt delivery, without a durable delivery queue. In-flight requests continue their opening attempt if the client disconnects, while the process remains running.

CLI exit codes:

| Code | Meaning |
| --- | --- |
| 0 | Successful command or no-op. |
| 1 | Argument, configuration, HTTP, or transport error. |
| 2 | Issue opening saved, but its notification attempt failed. |

`--json` returns the API result unchanged for successful calls and notification failures. Application errors produce `{"error":"…"}`; command-line parsing errors use the standard help/error text. A duplicate open succeeds with exit 0 even if the issue's previous notification failed. Use `get` to inspect historical outcomes.

## Development

```sh
cargo test --locked
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo build --release --locked
```

Commit `Cargo.lock` for reproducible dependency resolution. Tests use temporary databases and fake notification channels/local HTTP servers; they do not contact Pushover or send real alerts. Coverage includes lifecycle transitions, concurrent calls, late outcomes, restarts, YAML validation, authentication, CLI behavior, and provider failures.
