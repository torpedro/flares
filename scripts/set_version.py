"""Set the coordinated release version in every place it is written (Python 3.11+).

Usage: python scripts/set_version.py 0.2.0

Rewrites the five version strings, refreshes both lockfiles and the API snapshot,
then runs the checks in check_versions.py. Run it from a clean working tree so the
resulting diff is reviewable.
"""

import re
import subprocess
import sys

from check_versions import ROOT
from check_versions import main as check_versions

# Each entry is a file, a pattern whose two groups bracket the version string, and
# the number of matches expected. The workspace members and clients/rust inherit
# their versions, so they are not listed here; check_versions.py asserts that.
SITES = (
    ("Cargo.toml", r'(\[workspace\.package\]\nversion = ")[^"]+(")', 1),
    ("Cargo.toml", r'(^flares-(?:types|client) = \{ path = "[^"]+", version = ")[^"]+(")', 2),
    ("clients/python/src/flares_client/__init__.py", r'(^__version__ = ")[^"]+(")', 1),
    ("clients/bash/flares.sh", r"(^FLARES_CLIENT_VERSION=).+($)", 1),
)


def run(*args):
    return subprocess.run(args, cwd=ROOT, check=True)


def main():
    if len(sys.argv) != 2 or not re.fullmatch(r"\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?", sys.argv[1]):
        sys.exit("Usage: python scripts/set_version.py <major.minor.patch>")
    version = sys.argv[1]

    # Rewrite everything in memory first, so a pattern that stops matching cannot
    # leave the tree half-bumped.
    pending = {}
    for path, pattern, expected in SITES:
        text = pending.get(path) or (ROOT / path).read_text()
        text, count = re.subn(pattern, rf"\g<1>{version}\g<2>", text, flags=re.M)
        assert count == expected, f"{path}: replaced {count}, expected {expected}"
        pending[path] = text
    for path, text in pending.items():
        (ROOT / path).write_text(text)

    # Cargo.lock and uv.lock record the workspace versions; the OpenAPI snapshot
    # carries it in info.version via CARGO_PKG_VERSION.
    run("cargo", "check", "--workspace")
    run("uv", "lock", "--project", "clients/python")
    snapshot = subprocess.run(
        ("cargo", "run", "--locked", "--example", "export_openapi"),
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    (ROOT / "api/openapi.json").write_text(snapshot)

    check_versions()
    print(f"Set {version}. Review the diff, then commit.")


if __name__ == "__main__":
    main()
