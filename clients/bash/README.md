# Flare Bash client

Requires Bash, curl (with `--header @file` support), and jq. Source the versioned `flare.sh` file; no Rust or Python runtime is required.

```bash
source ./flare.sh
export FLARE_BASE_URL=http://localhost:8000
export FLARE_API_TOKEN=your-api-token
export FLARE_TIMEOUT=15

flare_open_issue backup --title "Backup failed" --severity critical
flare_get_issue backup
flare_list_issues open 100 0
flare_close_issue backup

flare_alert "Backup" "Finished" --idempotency-key backup-run-123
```

`FLARE_BASE_URL` defaults to `http://127.0.0.1:8000`, `FLARE_TIMEOUT` to 15 seconds, and `FLARE_API_TOKEN` is required. These are shell variables; exporting them is optional when functions run in the current shell.

| Function | Arguments |
| --- | --- |
| `flare_open_issue` | ID, optional `--title TEXT`, `--message TEXT`, `--severity LEVEL`, `--remind-every-seconds SECONDS`, `--notify-on-resolution` |
| `flare_close_issue`, `flare_get_issue` | Issue ID |
| `flare_list_issues` | Optional status, limit, offset |
| `flare_alert` | Title, message, optional `--severity LEVEL`, `--group-key KEY`, `--idempotency-key KEY` |
| `flare_get_delivery` | Numeric delivery ID |
| `flare_register_heartbeat` | JSON body |
| `flare_check_in`, `flare_delete_heartbeat` | Heartbeat ID |
| `flare_list_heartbeats`, `flare_health`, `flare_readiness`, `flare_metrics` | No arguments |

`flare_open_issue` and `flare_alert` accept quoted text arguments and safely build JSON internally. Severity defaults to `warning` and accepts `info`, `warning`, or `critical`. Opening an issue with only its ID uses server defaults for the title and message. Optional flags follow the required positional arguments.

```bash
flare_open_issue backup --title "Backup failed" --message "Check the logs" \
  --remind-every-seconds 3600 --notify-on-resolution
flare_alert "Deploy" "Deployment completed" --severity info --group-key deployments
```

These signatures replace the previous JSON-body arguments. `flare_register_heartbeat` still accepts a JSON body using the documented HTTP fields. Create that body with `jq --arg`; never interpolate user values into JSON or shell code. Requests are validated by the server.

Functions print JSON to stdout (`flare_metrics` prints Prometheus text). Diagnostics go to stderr. Return codes: 0 for success/no-op/pending; 1 for argument, HTTP, transport, or response errors; 2 when an alert or issue transition was saved but notification delivery failed. Inspection commands return 0 even if the inspected delivery previously failed.

When using `set -e`, call mutations in a conditional if you want to handle exit 2:

```bash
if result=$(flare_alert "Backup" "Finished" --idempotency-key backup-run-123); then
  printf '%s\n' "$result"
else
  code=$?
  printf 'Flare returned %s: %s\n' "$code" "$result" >&2
fi
```

The library preserves caller options and traps, does not use `eval`, disables curl configuration-file loading, and does not retry or follow redirects. Token headers are passed through a private temporary file, cleaned up when the request ends. Reuse an explicit alert idempotency key after an uncertain outcome.
