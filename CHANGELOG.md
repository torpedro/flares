# Changelog

## Unreleased

- Bash `flare_open_issue` and `flare_alert` now accept text arguments and named options instead of JSON bodies.

- Add independent Rust, Python (sync and async), and Bash clients for all Flare API operations.
- Share Rust API models through `flare-types`; the CLI uses `flare-client`.
- Check in the generated OpenAPI contract and test all clients against common scenarios.
- Build separate client artifacts with coordinated versions and CI checks.
