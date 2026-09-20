# Manual releases

Publishing is manual. CI tests every branch, pull request, and tag; tags and manual
workflow runs also build downloadable artifacts. Dependabot opens weekly dependency
update pull requests. Neither workflow publishes packages or creates GitHub Releases.

The examples below cut `0.1.0`. Substitute the new version consistently for later
releases. Use the same commit for every package, publish before tagging, and never
move a published release tag.

[sundiald](https://github.com/torpedro/sundiald) depends on `flares-client` from
crates.io. If a sundiald release is waiting on client changes, publish this release
first, then bump the dependency there.

## One-time setup

Verify access to `torpedro/flares`, crates.io, and PyPI. The Python project is
`flares-client`; the Rust crates are `flares-types`, `flares-client`, and `flares`.
First publication claims each available package name.

Create a crates.io API token with permission to publish all three crates, then run
`cargo login` and paste the token at its prompt. Create a PyPI API token separately.
For a new project the initial PyPI token needs permission to create it; once the
project exists, use a project-scoped token. Keep both tokens out of the repository,
out of command arguments, and out of shell history.

## 1. Choose the version

One workspace version covers the server, both Rust client crates, the Python client,
and the Bash client. Use this project policy:

| Change | Version example |
| --- | --- |
| Compatible fixes or features before 1.0 | `0.1.0` → `0.1.1` |
| Breaking changes before 1.0 | `0.1.1` → `0.2.0` |
| First stable public interface | `0.x.y` → `1.0.0` |
| Compatible fixes after 1.0 | `1.0.0` → `1.0.1` |
| Compatible features after 1.0 | `1.0.1` → `1.1.0` |
| Breaking changes after 1.0 | `1.x.y` → `2.0.0` |

Consider the HTTP API, CLI arguments, YAML configuration, the client library
interfaces in all four languages, and persisted database state when assessing
compatibility. A breaking change in any one of them bumps the shared version.
Document migration steps for breaking changes.

## 2. Prepare and test

Start from the intended release branch with unrelated changes committed or set aside.
Update the README, docs, and examples for any changed behavior. Make sure
`CHANGELOG.md` records everything in this release under `## Unreleased`, including
migration steps for breaking changes; those entries become the release notes.

Set the version everywhere in one step:

```bash
./scripts/make_release.sh
```

It shows the current version, offers the next patch/minor/major or a custom one,
updates every file, offers to move the changelog entries under the new heading, and
offers to commit. It then runs the packaging dry run, offers to build the release
artifacts (step 3), publish the crates and the Python distributions (step 4), and tag
(step 5), in that order, so you can drive the whole release from it or stop at any
prompt.

To set a version non-interactively instead:

```bash
uv run --project clients/python python scripts/set_version.py 0.1.0
```

That rewrites the workspace version and the published versions of the workspace's own
crates in `Cargo.toml`, `__version__` in the Python client, and `FLARES_CLIENT_VERSION`
in the Bash client. It then refreshes `Cargo.lock` and `clients/python/uv.lock`,
regenerates `api/openapi.json`, and runs `scripts/check_versions.py` to confirm every
package and the API agree. The workspace members and `clients/rust` inherit their
versions and are not edited; `clients/python/pyproject.toml` reads its version from
`__init__.py`. Run it from a clean working tree and review the diff.

If you declined the script's commit prompt, commit all intended changes. Push the
commit and wait for CI to pass. The working tree must be clean before publishing;
don't use `--allow-dirty` to bypass Cargo's publishing checks.

## 3. Verify the package and build artifacts

CI runs `cargo publish --workspace --dry-run --locked` on every push, so packaging
is already validated for the commit you just pushed. To check it locally as well:

```bash
cargo publish --workspace --dry-run --locked --registry crates-io
```

Cargo packages the three crates in dependency order and builds each extracted crate,
resolving the intra-workspace dependencies that are not yet on crates.io.

Then build the release artifacts. Trigger a `workflow_dispatch` run of CI on the
branch holding the release commit, confirm the run used that exact commit, and
download its `flares-linux-x86_64-and-clients` artifact into `dist/`. It contains
the Linux x86_64 server/CLI binary archive, both Rust library crates, the Python
wheel and source distribution, and the Bash archive.

Alternatively, build from the clean release commit locally:

```bash
uv run --project clients/python python scripts/build_artifacts.py
```

The local binary targets the build machine; it is not a universal executable. The CI
binary is built on `ubuntu-latest` and is not a static Linux binary. Use the
appropriate platform label in the GitHub Release.

## 4. Publish

From the release checkout, publish the Rust crates:

```bash
cargo publish --workspace --locked --registry crates-io
```

Cargo uploads `flares-types`, `flares-client`, and `flares` in dependency order and
waits for each to become available before publishing its consumers. If Cargo times
out waiting for the index, check crates.io before retrying: the upload may already
have succeeded. Published versions cannot be overwritten.

Then publish the Python distributions. `make_release.sh` offers this directly after
the crates, reading the token the same way. To do it by hand, read the token without
displaying it or putting it in shell history:

```bash
read -r -s -p 'PyPI token: ' UV_PUBLISH_TOKEN
printf '\n'
export UV_PUBLISH_TOKEN
uv publish dist/flares_client-0.1.0-py3-none-any.whl dist/flares_client-0.1.0.tar.gz
unset UV_PUBLISH_TOKEN
```

Run `unset` even if the upload fails. Upload only these two files, not `dist/*`,
which contains other package types and can contain stale builds.

## 5. Tag and announce

After confirming publication on crates.io and PyPI, tag the published commit:

```bash
git tag -a v0.1.0 -m "Flares 0.1.0"
git push origin v0.1.0
```

Check the tag name against the workspace version before pushing. `check_versions.py`
asserts that they agree, but only on tag runs, so that check now lands after
publication rather than before it.

Give the binary download a platform-specific name and create checksums. These
commands assume you are at the repository root:

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
`Flares v0.1.0`, and attach those six files plus `SHA256SUMS`. Use this version's
changelog entries as its description, include installation commands and the binary's
platform requirements, review the draft, then publish it.

## 6. Verify the public installation

Use a fresh directory/environment so the source checkout cannot mask missing files:

```bash
cargo install flares --version 0.1.0 --locked
flares --version
uv run --isolated --no-project --with flares-client==0.1.0 python -c \
  'import flares_client; print(flares_client.__version__)'
```

Download the Bash archive from the public release, extract it, and source
`flares-bash-0.1.0/flares.sh`. Verify `FLARES_CLIENT_VERSION` is `0.1.0`. Also
download the binary archive and confirm its `flares --version` output.

## Correcting a published release

Registry versions are immutable. Fix the issue and repeat this process with a new
version. Preserve existing tags so each continues to identify the source that was
published.

If a later publication in step 4 fails, the earlier packages are already public.
Keep the same version and resume at the first unpublished package: pass
`-p <crate>` to `cargo publish` for the remaining crates, or upload only the missing
Python distribution. Do not rebuild different contents under an already published
version. If a code change is needed, cut a new version.

If a release has a serious defect, consider yanking that version:

```bash
cargo yank flares --version 0.1.0 --registry crates-io
```

Yank each affected crate separately. Yanking does not uninstall existing binaries or
remove the published source. Explain the issue and replacement version in the release
notes.

References: [Cargo publishing](https://doc.rust-lang.org/cargo/commands/cargo-publish.html),
[uv publishing](https://docs.astral.sh/uv/guides/package/).
