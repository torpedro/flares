# HTTP contract

`openapi.json` is generated from the server's Utoipa annotations and shared Rust types.
Change the server annotations/types first, then regenerate:

```sh
cargo run --locked --example export_openapi > api/openapi.json
```

The Rust snapshot test rejects contract drift. Client implementations are handwritten;
`tests/contract/scenarios.json` and the failure tests exercise their common behavior
against a temporary server and local fake notification provider.
