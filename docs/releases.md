# Manual releases

Publishing is manual. CI tests every branch, pull request, and tag; tags and manual
workflow runs also build downloadable artifacts. Dependabot opens weekly dependency
update pull requests. Neither workflow publishes packages or creates GitHub Releases.

The examples below cut `0.1.0`. Substitute the new version consistently for later
releases. Use the same commit for every package and never move a published release tag.

## 1. Prepare the release commit

Verify access to `torpedro/flares`, crates.io, and PyPI. The Python project is
`flares-client`; the Rust crates are `flares-types`, `flares-client`, and `flares`.
First publication claims each available package name.

Update these together if changing the version:

- `Cargo.toml`: workspace version and local dependency versions.
- `clients/rust/Cargo.toml`: local `flares-types` dependency version.
- `clients/python/pyproject.toml` and `src/flares_client/__init__.py` beneath it.
- `clients/bash/flares.sh`: `FLARES_CLIENT_VERSION`.
- `CHANGELOG.md`: move release entries under a dated version heading.

Refresh lockfiles and the API snapshot when necessary:

```bash
cargo check --workspace
uv lock --project clients/python
cargo run --locked --example export_openapi > api/openapi.json
uv run --project clients/python python scripts/check_versions.py
```

Review and commit all intended changes, push the commit, and wait for CI to pass.
The working tree must be clean before publishing; don't use `--allow-dirty` to
bypass Cargo's publishing checks.

## 2. Tag and build

```bash
git tag -a v0.1.0 -m "Flares 0.1.0"
git push origin v0.1.0
```

Wait for the tag's CI run to pass. Download its `flares-linux-x86_64-and-clients`
artifact from GitHub Actions and extract it into `dist/`. It includes the Linux
x86_64 server/CLI binary archive, both Rust library crates, Python wheel and source
distribution, and Bash archive. Use the tag run, not an artifact from another commit.

Alternatively, build from the clean tagged commit locally:

```bash
uv run --project clients/python python scripts/build_artifacts.py
```

The local binary targets the build machine; it is not a universal executable.
The CI binary is built on `ubuntu-latest` and is not a static Linux binary.
Use the appropriate platform label in the GitHub Release.

## 3. Publish Rust packages

Create a crates.io API token with permission to publish these crates, then run
`cargo login` and paste the token at its prompt. Do not put it in a command argument
or commit it. From the release checkout, run these commands individually:

```bash
cargo publish -p flares-types --locked --dry-run
cargo publish -p flares-types --locked

cargo publish -p flares-client --locked --dry-run
cargo publish -p flares-client --locked

cargo publish -p flares --locked --dry-run
cargo publish -p flares --locked
```

Wait for each publication to succeed before proceeding: dependencies must be
available on crates.io before publishing their consumers. `--dry-run` validates
packaging without uploading; the next command uploads that crate.

## 4. Publish Python

Create a PyPI API token. For a new project, the initial token needs permission to
create it; once the project exists, use a project-scoped token. In Bash, read it
without displaying it or putting it in shell history:

```bash
read -r -s -p 'PyPI token: ' UV_PUBLISH_TOKEN
printf '\n'
export UV_PUBLISH_TOKEN
uv publish dist/flares_client-0.1.0-py3-none-any.whl dist/flares_client-0.1.0.tar.gz
unset UV_PUBLISH_TOKEN
```

Run `unset` even if the upload fails. Upload only these two files, not `dist/*`,
which contains other package types and can contain stale builds.

## 5. Create the GitHub Release

Using the tag's Linux CI artifact, give the binary download a platform-specific name
and create checksums. These commands assume you are at the repository root:

```bash
cp dist/flares-0.1.0.tar.gz dist/flares-0.1.0-linux-x86_64.tar.gz
(
  cd dist
  sha256sum flares-0.1.0-linux-x86_64.tar.gz flares-bash-0.1.0.tar.gz \
    flares-types-0.1.0.crate flares-client-0.1.0.crate \
    flares_client-0.1.0-py3-none-any.whl flares_client-0.1.0.tar.gz > SHA256SUMS
)
```

On GitHub, create a draft release for existing tag `v0.1.0`, title it
`Flares v0.1.0`, and attach those six files plus `SHA256SUMS`. Copy the relevant
changelog entries into its description, include installation commands and the
binary's platform requirements, review the draft, then publish it.

## 6. Verify the public installation paths

Use a fresh directory/environment so the source checkout cannot mask missing files:

```bash
cargo install flares --version 0.1.0 --locked
flares --version
uv run --isolated --no-project --with flares-client==0.1.0 python -c \
  'import flares_client; print(flares_client.__version__)'
```

Download the Bash archive from the public release, extract it, and source
`flares-bash-0.1.0/flares.sh`. Verify `FLARES_CLIENT_VERSION` is `0.1.0`.
Also download the binary archive and confirm its `flares --version` output.

## Recovering a partial release

Registry versions are immutable. If a later publication fails, keep the same tag
and resume at the first unpublished package. For a partially uploaded Python
release, upload only the missing distribution. Do not rebuild different contents
under an already published version. If a code change is needed, cut a new version.

References: [Cargo publishing](https://doc.rust-lang.org/cargo/commands/cargo-publish.html),
[uv publishing](https://docs.astral.sh/uv/guides/package/).
