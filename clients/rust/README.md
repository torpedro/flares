# Flare Rust client

An async HTTP client with typed requests, responses, and errors. It uses reqwest and a Tokio runtime; it does not depend on the Flare server, SQLite, Axum, or Clap.

```rust,no_run
use flare_client::{ApiClient, OpenIssue, Error};

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

Add `flare-client` as a path dependency on `clients/rust` when using the source checkout, or use the versioned crate artifact. API types are re-exported from `flare-types`.

Operations: `open`, `close`, `get`, `list`, `alert`, `delivery`, `register_heartbeat`, `check_in`, `heartbeats`, `delete_heartbeat`, `health`, `readiness`, and `metrics`. `ApiClient` is cloneable and shares its connection pool. `with_timeout(url, token, Duration)` overrides the default 15-second request deadline.

Match `Error::Validation`, `Error::Transport`, `Error::Http { status }`, and `Error::Decode`. Errors omit credentials, request URLs, and response bodies. HTTP success with a failed notification returns a normal typed result. Pending deliveries are accepted but not yet delivered. Requests never retry or follow redirects automatically; use an explicit idempotency key with `alert` when a request may be retried.

Build independently with `cargo check -p flare-client --locked`. Client users need no server configuration files. The CLI adapts its existing YAML settings to this library.
