"""Check versions before building or tagging the coordinated release (Python 3.11+)."""

import json
import os
import re
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def manifest(path):
    return tomllib.loads((ROOT / path).read_text())


def main():
    cargo = manifest("Cargo.toml")
    version = cargo["workspace"]["package"]["version"]
    for path in ("Cargo.toml", "crates/flare-types/Cargo.toml", "clients/rust/Cargo.toml"):
        data = manifest(path)
        assert data["package"]["version"] == {"workspace": True}, path
        for name, dep in data.get("dependencies", {}).items():
            if isinstance(dep, dict) and "path" in dep:
                assert dep["version"] == version, (path, name)
    assert manifest("clients/python/pyproject.toml")["project"]["version"] == version
    python = (ROOT / "clients/python/src/flare_client/__init__.py").read_text()
    assert re.search(r'^__version__ = "([^"]+)"', python, re.M)[1] == version
    bash = (ROOT / "clients/bash/flare.sh").read_text()
    assert re.search(r"^FLARE_CLIENT_VERSION=(.+)$", bash, re.M)[1] == version
    assert json.loads((ROOT / "api/openapi.json").read_text())["info"]["version"] == version
    if os.environ.get("GITHUB_REF_TYPE") == "tag":
        assert os.environ["GITHUB_REF_NAME"] == f"v{version}", "Tag must match package versions"
    print(f"All packages and API agree on {version}")


if __name__ == "__main__":
    main()
