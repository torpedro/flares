"""File configuration follows the same fixtures and path rules as the Rust SDK/CLI."""

import asyncio
import json
from pathlib import Path

import pytest
from flares_client import AsyncClient, Client, ValidationError
from flares_client._config import _find_config, load_config

CASES = json.loads(
    (Path(__file__).resolve().parents[2] / "rust/tests/fixtures/client-config.json").read_text()
)


@pytest.mark.parametrize("case", CASES, ids=lambda case: case["id"])
def test_shared_configuration_cases(tmp_path, case):
    configs = tmp_path / "configs"
    configs.mkdir()
    for directory in (tmp_path, configs):
        (directory / "token").write_bytes(b"test-token\r\n")
    path = configs / "client.yaml"
    path.write_text(case["yaml"])
    if "expected" in case:
        expected = case["expected"]
        assert load_config(path) == (expected["base_url"], expected["token"], expected["timeout"])
        with Client.from_config(path) as client:
            assert client._client.headers["Authorization"] == f"Bearer {expected['token']}"
            assert client._client.timeout.read == expected["timeout"]

        async def check_async():
            async with AsyncClient.from_config(path) as client:
                assert client._client.headers["Authorization"] == f"Bearer {expected['token']}"
                assert client._client.timeout.read == expected["timeout"]

        asyncio.run(check_async())
    else:
        for factory in (load_config, Client.from_config, AsyncClient.from_config):
            with pytest.raises(ValidationError) as error:
                factory(path)
            assert str(path) in str(error.value)
            assert "sensitive" not in str(error.value)


def test_discovery_precedence_and_missing_files(tmp_path):
    xdg = tmp_path / "xdg"
    home = tmp_path / "home"
    system = tmp_path / "etc/flares"
    user = home / ".config/flares/client.yaml"
    xdg_file = xdg / "flares/client.yaml"
    for path in (user, xdg_file, system / "client.yaml"):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("api_token: token\n")
    assert _find_config(str(xdg), str(home), system) == xdg_file
    assert _find_config(None, str(home), system) == user
    assert _find_config("", str(home), system) == user
    assert _find_config("relative", str(home), system) == user
    assert _find_config(None, None, system) == system / "client.yaml"
    assert _find_config(None, "", system) == system / "client.yaml"
    xdg_file.unlink()
    # XDG replaces HOME; it is not another configuration layer.
    assert _find_config(str(xdg), str(home), system) == system / "client.yaml"
    user.unlink()
    (system / "client.yaml").unlink()
    with pytest.raises(ValidationError, match="No client.yaml found"):
        _find_config(str(xdg), str(home), system)


@pytest.mark.parametrize("client_type", (Client, AsyncClient))
def test_opt_in_discovery_and_environment_secret(tmp_path, monkeypatch, client_type):
    configs = tmp_path / "xdg/flares"
    configs.mkdir(parents=True)
    path = configs / "client.yaml"
    path.write_text("api_token: {env: FLARES_CONFIG_TEST_TOKEN}\n")
    monkeypatch.setenv("XDG_CONFIG_HOME", str(configs.parent))
    monkeypatch.delenv("HOME", raising=False)
    monkeypatch.setenv("FLARES_CONFIG_TEST_TOKEN", "environment-secret")
    client = client_type.from_default_config()
    assert client._client.headers["Authorization"] == "Bearer environment-secret"
    if client_type is Client:
        client.close()
    else:
        asyncio.run(client.aclose())
    path.write_text("api_token: [sensitive\n")
    with pytest.raises(ValidationError, match="invalid configuration YAML"):
        client_type.from_default_config()
    # Explicit constructors do not discover or load the invalid config.
    client = client_type("http://127.0.0.1:8000", "explicit-token")
    if client_type is Client:
        client.close()
    else:
        asyncio.run(client.aclose())


def test_symlink_and_absolute_secret_paths(tmp_path, monkeypatch):
    target = tmp_path / "target"
    target.mkdir()
    (target / "token").write_text("target-token\n")
    path = target / "client.yaml"
    path.write_text("api_token: {file: token}\n")
    (tmp_path / "token").write_text("invalid token")
    link = tmp_path / "client.yaml"
    link.symlink_to(path)
    monkeypatch.chdir(tmp_path)
    assert load_config("client.yaml")[1] == "target-token"
    path.write_text(f"api_token: {{file: '{target / 'token'}'}}\n")
    assert load_config(link)[1] == "target-token"


def test_broken_symlink_does_not_fall_back(tmp_path):
    user = tmp_path / "flares/client.yaml"
    user.parent.mkdir()
    user.symlink_to(tmp_path / "missing")
    assert _find_config(str(tmp_path), None, tmp_path / "system") == user
    with pytest.raises(ValidationError, match="cannot read"):
        load_config(user)


def test_secret_errors_omit_values_and_preserve_whitespace(tmp_path, monkeypatch):
    path = tmp_path / "client.yaml"
    path.write_text("api_token: {file: token}\n")
    for content in (b"sensitive-token \n", b"sensitive\rvalue", b"\xffsensitive"):
        (tmp_path / "token").write_bytes(content)
        with pytest.raises(ValidationError) as error:
            load_config(path)
        assert "sensitive" not in str(error.value)
    path.write_text("api_token: {env: FLARES_CONFIG_TEST_TOKEN}\n")
    monkeypatch.delenv("FLARES_CONFIG_TEST_TOKEN", raising=False)
    with pytest.raises(ValidationError, match="api_token"):
        load_config(path)
    monkeypatch.setenv("FLARES_CONFIG_TEST_TOKEN", "sensitive value")
    with pytest.raises(ValidationError) as error:
        load_config(path)
    assert "sensitive" not in str(error.value)
