# Changelog

## Unreleased

## 0.1.2 - 2026-09-20

- Add explicit YAML loading and default discovery to Rust and Python clients, using the same client format, secret references, and config-relative paths as the CLI.
- Honor absolute `XDG_CONFIG_HOME` for server/client discovery, with HOME and system-directory fallbacks; include selected filenames in loading errors.

## 0.1.1 - 2026-09-20

- Add `scripts/make_release.sh`, an interactive helper that bumps the version, updates the changelog, verifies packaging, and optionally commits, builds the release artifacts, publishes to crates.io and PyPI, tags, and pushes the tag.

- Verify packaging of all three crates in CI on every run, and publish them with a single `cargo publish --workspace` in the release process.

- Look up server and client configuration in `~/.config/flares`, then `/etc/flares`, instead of the current directory. `--config` remains an explicit override.

- Upgrade rusqlite to 0.40.2, retaining bundled SQLite and enabling checked unsigned integer conversions for counts and rate windows.

- Remove legacy YAML aliases, top-level Pushover, nested Pushover `config`, and numeric duration compatibility. Server configuration requires `server`; all YAML durations require unit strings such as `15s`.

- Rename the project and binary to `flares`, crates to `flares-client`/`flares-types`, Python imports to `flares_client`, and Bash functions/environment variables to `flares_*`/`FLARES_*`.
- The default database is now `flares.sqlite3`; existing installations should explicitly keep their previous database path. Metrics use the `flares_` prefix. Webhook idempotency keys retain the original prefix so pending retries remain deduplicated.

- Add structured server/storage/delivery/routing YAML, duration strings, and environment/file secret references, using the structured format.
- Add `flares config check` and redacted `flares config show`, including client configuration support.
- Respect explicit empty routing defaults; configure Pushover as a named destination.
- Add independent destination retry/rate policies, a configurable active queue limit, and opt-in delivery/key retention.

- Bash `flares_open_issue` and `flares_alert` now accept text arguments and named options instead of JSON bodies.

- Add independent Rust, Python (sync and async), and Bash clients for all Flares API operations.
- Share Rust API models through `flares-types`; the CLI uses `flares-client`.
- Check in the generated OpenAPI contract and test all clients against common scenarios.
- Build separate client artifacts with coordinated versions and CI checks.
