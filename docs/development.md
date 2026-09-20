# Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --examples --locked
cargo build --bin flares --locked
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
CI also runs ShellCheck and detects OpenAPI/version drift. Tags and manual workflow
runs build package artifacts; ordinary branch and pull request runs execute checks only.

