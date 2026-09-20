# Flares API types

Shared serializable HTTP request and response types for the server and Rust client. Default dependencies are Serde and Chrono. The optional `openapi` feature enables Utoipa schema derives; `cli` enables Clap enum derives. Applications normally obtain these types through `flares-client`.
