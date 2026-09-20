"""Build separate release artifacts without publishing (run from the repository root)."""

import hashlib
import platform
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


def platform_tag():
    """Label the binary archive for the machine that built it; it is not portable."""
    machine = platform.machine().lower()
    return f"{platform.system().lower()}-{'x86_64' if machine == 'amd64' else machine}"


def write_checksums(dist):
    """Checksum every artifact so local and CI builds produce identical dist/ trees."""
    # uv build leaves a .gitignore in the output directory; it is not an artifact.
    names = sorted(
        p.name
        for p in dist.iterdir()
        if p.is_file() and p.name != "SHA256SUMS" and not p.name.startswith(".")
    )
    lines = [
        f"{hashlib.sha256((dist / name).read_bytes()).hexdigest()}  {name}\n" for name in names
    ]
    (dist / "SHA256SUMS").write_text("".join(lines))
    return names


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
    with tarfile.open(dist / f"flares-{version}-{platform_tag()}.tar.gz", "w:gz") as archive:
        archive.add(ROOT / "target/release/flares", arcname="flares")
    for name in write_checksums(dist):
        print(name)
    print(f"Artifacts built and checked in {dist}")


if __name__ == "__main__":
    main()
