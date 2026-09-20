# Flares Rust client

An async HTTP client with typed requests, responses, and errors. It uses reqwest and a Tokio runtime; it does not depend on the Flares server, SQLite, Axum, or Clap.

```rust,no_run
use flares_client::{ApiClient, OpenIssue, Error};

# async fn example() -> Result<(), Error> {
let client = ApiClient::new("http://localhost:8000", "your-api-token")?;
let result = client.open(OpenIssue {
    id: "backup".into(),
    title: Some("Backup failed".into()),
    ..Default::default()
}).await?;
println!("{:?}", result.notification.status);
client.close("backup".into()).await?;
# Ok(())
# }
```

Add `flares-client` as a path dependency on `clients/rust` when using the source checkout, or use the versioned crate artifact. API types are re-exported from `flares-types`.

Operations: `open`, `close`, `get`, `list`, `alert`, `delivery`, `register_heartbeat`, `check_in`, `heartbeats`, `delete_heartbeat`, `health`, `readiness`, and `metrics`. `ApiClient` is cloneable and shares its connection pool. `with_timeout(url, token, Duration)` overrides the default 15-second request deadline.

Match `Error::Configuration`, `Error::Validation`, `Error::Transport`, `Error::Http { status }`, and `Error::Decode`. Errors omit credentials, request URLs, and response bodies. HTTP success with a failed notification returns a normal typed result. Pending deliveries are accepted but not yet delivered. Requests never retry or follow redirects automatically; use an explicit idempotency key with `alert` when a request may be retried.

Build independently with `cargo check -p flares-client --locked`. Client users need no server configuration files. The CLI shares this library's client YAML loader.

Load an explicit client file or opt into discovery:

```rust,no_run
use flares_client::{ApiClient, Error};
# fn example() -> Result<(), Error> {
let client = ApiClient::from_config("/path/client.yaml")?;
let client = ApiClient::from_default_config()?;
# Ok(())
# }
```

Client YAML accepts `base_url` (default `http://127.0.0.1:8000`), required `api_token`,
and `timeout` (default `15s`; whole-number durations with `s`, `m`, `h`, `d`, or `w`).
Tokens accept a literal string, `{env: NAME}`, or `{file: PATH}`. Relative secret
paths use the config file's canonical directory, including the target directory
for symlinked configs. Relative file arguments use the process working directory.

Discovery selects `$XDG_CONFIG_HOME/flares/client.yaml` when XDG is absolute,
otherwise `$HOME/.config/flares/client.yaml`, then `/etc/flares/client.yaml`.
Empty or relative XDG values are ignored; missing/empty HOME skips that location.
Files are never merged. Invalid or unreadable selected files fail without fallback.
An explicit path never falls back. Ordinary constructors do not read configuration
files or environment settings. Loading errors include the selected file and omit
secret values. See the [configuration guide](../../docs/configuration.md).
