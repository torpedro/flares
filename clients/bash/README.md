# Flares Bash client

Requires Bash, curl (with `--header @file` support), and jq. Source the versioned `flares.sh` file; no Rust or Python runtime is required.

```bash
source ./flares.sh
export FLARES_BASE_URL=http://localhost:8000
export FLARES_API_TOKEN=your-api-token
export FLARES_TIMEOUT=15

flares_open_issue backup --title "Backup failed" --severity critical
flares_get_issue backup
flares_list_issues open 100 0
flares_close_issue backup

flares_alert "Backup" "Finished" --idempotency-key backup-run-123
```

`FLARES_BASE_URL` defaults to `http://127.0.0.1:8000`, `FLARES_TIMEOUT` to 15 seconds, and `FLARES_API_TOKEN` is required. These are shell variables; exporting them is optional when functions run in the current shell.

| Function | Arguments |
| --- | --- |
| `flares_open_issue` | ID, optional `--title TEXT`, `--message TEXT`, `--severity LEVEL`, `--remind-every-seconds SECONDS`, `--notify-on-resolution` |
| `flares_close_issue`, `flares_get_issue` | Issue ID |
| `flares_list_issues` | Optional status, limit, offset |
| `flares_alert` | Title, message, optional `--severity LEVEL`, `--group-key KEY`, `--idempotency-key KEY` |
| `flares_get_delivery` | Numeric delivery ID |
| `flares_register_heartbeat` | JSON body |
| `flares_check_in`, `flares_delete_heartbeat` | Heartbeat ID |
| `flares_list_heartbeats`, `flares_health`, `flares_readiness`, `flares_metrics` | No arguments |

`flares_open_issue` and `flares_alert` accept quoted text arguments and safely build JSON internally. Severity defaults to `warning` and accepts `info`, `warning`, or `critical`. Opening an issue with only its ID uses server defaults for the title and message. Optional flags follow the required positional arguments.

```bash
flares_open_issue backup --title "Backup failed" --message "Check the logs" \
  --remind-every-seconds 3600 --notify-on-resolution
flares_alert "Deploy" "Deployment completed" --severity info --group-key deployments
```

These signatures replace the previous JSON-body arguments. `flares_register_heartbeat` still accepts a JSON body using the documented HTTP fields. Create that body with `jq --arg`; never interpolate user values into JSON or shell code. Requests are validated by the server.

Functions print JSON to stdout (`flares_metrics` prints Prometheus text). Diagnostics go to stderr. Return codes: 0 for success/no-op/pending; 1 for argument, HTTP, transport, or response errors; 2 when an alert or issue transition was saved but notification delivery failed. Inspection commands return 0 even if the inspected delivery previously failed.

When using `set -e`, call mutations in a conditional if you want to handle exit 2:

```bash
if result=$(flares_alert "Backup" "Finished" --idempotency-key backup-run-123); then
  printf '%s\n' "$result"
else
  code=$?
  printf 'Flares returned %s: %s\n' "$code" "$result" >&2
fi
```

The library preserves caller options and traps, does not use `eval`, disables curl configuration-file loading, and does not retry or follow redirects. Token headers are passed through a private temporary file, cleaned up when the request ends. Reuse an explicit alert idempotency key after an uncertain outcome.
