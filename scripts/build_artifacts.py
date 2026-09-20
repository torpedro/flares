"""Build separate release artifacts without publishing (run from the repository root)."""

import shutil
import subprocess
import tarfile
import tempfile
import tomllib
from pathlib import Path

from check_versions import ROOT
from check_versions import main as check_versions


def run(*args, **kwargs):
    return subprocess.run(args, cwd=ROOT, check=True, **kwargs)


def main():
    check_versions()
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    dist = ROOT / "dist"
    dist.mkdir(exist_ok=True)
    run("cargo", "build", "--release", "--locked", "--bin", "flares")
    # Cargo 1.94's temporary registry can fail verification of unpublished workspace
    # dependencies. Verify the exact extracted artifacts together below instead.
    run(
        "cargo",
        "package",
        "--workspace",
        "--exclude",
        "flares",
        "--locked",
        "--allow-dirty",
        "--no-verify",
    )
    archives = []
    for name in ("flares-types", "flares-client"):
        archive = ROOT / f"target/package/{name}-{version}.crate"
        shutil.copy2(archive, dist)
        archives.append(archive)
    with tempfile.TemporaryDirectory(prefix="flares-packages-") as directory:
        for archive in archives:
            with tarfile.open(archive) as package:
                package.extractall(directory, filter="data")
        types = Path(directory) / f"flares-types-{version}"
        client = Path(directory) / f"flares-client-{version}/Cargo.toml"
        patch = f'patch.crates-io.flares-types.path="{types}"'
        run(
            "cargo",
            "test",
            "--manifest-path",
            str(client),
            "--config",
            patch,
            "--target-dir",
            str(ROOT / "target/package-verify"),
        )
        tree = run(
            "cargo",
            "tree",
            "--manifest-path",
            str(client),
            "--config",
            patch,
            "--edges",
            "normal",
            "--prefix",
            "none",
            capture_output=True,
            text=True,
        ).stdout
        forbidden = {"axum", "rusqlite", "clap", "utoipa"}
        assert not forbidden.intersection(line.split()[0] for line in tree.splitlines())
    run("uv", "build", "--project", "clients/python", "--out-dir", str(dist))
    wheel = dist / f"flares_client-{version}-py3-none-any.whl"
    run(
        "uv",
        "run",
        "--isolated",
        "--no-project",
        "--with",
        str(wheel),
        "python",
        "-c",
        "import flares_client; from importlib.resources import files; "
        "assert files(flares_client).joinpath('py.typed').is_file(); "
        "client = flares_client.Client('http://127.0.0.1:1', 'token'); client.close()",
    )
    with tarfile.open(dist / f"flares-bash-{version}.tar.gz", "w:gz") as archive:
        for name in ("flares.sh", "README.md"):
            archive.add(ROOT / "clients/bash" / name, arcname=f"flares-bash-{version}/{name}")
    with tarfile.open(dist / f"flares-{version}.tar.gz", "w:gz") as archive:
        archive.add(ROOT / "target/release/flares", arcname="flares")
    print(f"Artifacts built and checked in {dist}")


if __name__ == "__main__":
    main()
