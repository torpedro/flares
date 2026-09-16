"""Run identical scenarios against a real Flare process using each packaged client."""

import asyncio
import contextlib
import http.server
import json
import os
import socket
import subprocess
import threading
import time
from pathlib import Path

import httpx
import pytest
from flare_client import AsyncClient, Client, HTTPError

ROOT = Path(__file__).resolve().parents[2]
SCENARIOS = json.loads(Path(__file__).with_name("scenarios.json").read_text())


@pytest.fixture
def server(tmp_path):
    class Provider(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            self.send_response(503 if body["title"] == "fail" else 204)
            self.end_headers()

        def log_message(self, *args):
            pass

    provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    thread = threading.Thread(target=provider.serve_forever, daemon=True)
    thread.start()
    with socket.socket() as reserved:
        reserved.bind(("127.0.0.1", 0))
        port = reserved.getsockname()[1]
    config = tmp_path / "server.yaml"
    config.write_text(
        f"port: {port}\napi_token: contract-token\ndatabase: issues.sqlite3\n"
        "delivery:\n  group_window_seconds: 60\n  rate_limit_per_minute: 0\n"
        "destinations:\n  test:\n    type: webhook\n"
        f"    url: http://127.0.0.1:{provider.server_port}/\n"
        "default_destinations: [test]\n"
    )
    process = subprocess.Popen(
        [str(ROOT / "target/debug/flare"), "serve", "--config", str(config)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    url = f"http://127.0.0.1:{port}"
    try:
        with httpx.Client(timeout=0.5) as probe:
            for _ in range(100):
                assert process.poll() is None, "Flare exited during startup"
                try:
                    if probe.get(url + "/readyz").is_success:
                        break
                except httpx.HTTPError:
                    pass
                time.sleep(0.05)
            else:
                pytest.fail("Flare did not become ready")
        yield {**os.environ, "FLARE_BASE_URL": url, "FLARE_API_TOKEN": "contract-token"}
    finally:
        process.terminate()
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        provider.shutdown()
        provider.server_close()
        thread.join()


def lookup(value, path):
    for key in path.split("."):
        value = value[int(key)] if isinstance(value, list) else value[key]
    return value


def resolve(value, saved):
    if isinstance(value, dict):
        if "$ref" in value:
            return lookup(saved, value["$ref"])
        return {k: resolve(v, saved) for k, v in value.items()}
    return value


def normalize(value):
    if hasattr(value, "model_dump"):
        return value.model_dump(mode="json")
    if isinstance(value, list):
        return [normalize(v) for v in value]
    return value


def shell(env, op, args):
    if op in ("open_issue", "register_heartbeat"):
        argv = [json.dumps(args)]
    elif op == "alert":
        args = dict(args)
        key = args.pop("idempotency_key", None)
        argv = [json.dumps(args)] + ([key] if key is not None else [])
    elif op == "list_issues":
        argv = [
            args.get("status", ""),
            str(args.get("limit", 100)),
            str(args.get("offset", 0)),
        ]
    elif "id" in args:
        argv = [str(args["id"])]
    else:
        argv = []
    # Positional arguments keep IDs/content out of shell code, including quotes and $().
    return subprocess.run(
        [
            "bash",
            "-euo",
            "pipefail",
            "-c",
            'source "$1"; shift; "$@"',
            "contract",
            str(ROOT / "clients/bash/flare.sh"),
            f"flare_{op}",
            *argv,
        ],
        env=env,
        capture_output=True,
        text=True,
        timeout=20,
    )


@pytest.mark.parametrize("backend", ["rust", "python", "python_async", "bash"])
def test_shared_contract(server, backend):
    saved = {}
    with contextlib.ExitStack() as stack:
        if backend == "python":
            client = stack.enter_context(
                Client(server["FLARE_BASE_URL"], server["FLARE_API_TOKEN"])
            )
        elif backend == "python_async":
            runner = stack.enter_context(asyncio.Runner())
            client = AsyncClient(server["FLARE_BASE_URL"], server["FLARE_API_TOKEN"])
            stack.callback(lambda: runner.run(client.aclose()))
        for step in SCENARIOS:
            op, args = step["op"], resolve(step["args"], saved)
            expected_error = step.get("http_error")
            if backend == "rust":
                output = subprocess.run(
                    [str(ROOT / "target/debug/examples/contract_driver")],
                    input=json.dumps({"op": op, "args": args}),
                    env=server,
                    capture_output=True,
                    text=True,
                    timeout=20,
                    check=True,
                )
                output = json.loads(output.stdout)
                if expected_error:
                    assert output == {"error": "http", "status": expected_error}
                    continue
                value = output["value"]
            elif backend == "bash":
                output = shell(server, op, args)
                if expected_error:
                    assert output.returncode == 1
                    assert f"HTTP {expected_error}" in output.stderr
                    continue
                assert output.returncode == step.get("bash_exit", 0), output.stderr
                value = output.stdout if op == "metrics" else json.loads(output.stdout)
            else:
                try:
                    value = getattr(client, op)(**args)
                    if backend == "python_async":
                        value = runner.run(value)
                    value = normalize(value)
                except HTTPError as error:
                    assert error.status_code == expected_error
                    continue
                assert expected_error is None
            for path, expected in step.get("expect", {}).items():
                assert lookup(value, path) == resolve(expected, saved), (
                    backend,
                    op,
                    path,
                    value,
                )
            if "contains" in step:
                assert step["contains"] in value
            if "save" in step:
                saved[step["save"]] = value


@pytest.mark.parametrize("backend", ["rust", "python", "python_async", "bash"])
def test_authentication_error(server, backend):
    env = {**server, "FLARE_API_TOKEN": "wrong"}
    if backend == "bash":
        output = shell(env, "list_issues", {})
        assert output.returncode == 1 and "401" in output.stderr
    elif backend == "rust":
        output = subprocess.run(
            [str(ROOT / "target/debug/examples/contract_driver")],
            input='{"op":"list_issues","args":{}}',
            env=env,
            text=True,
            capture_output=True,
            check=True,
        )
        assert json.loads(output.stdout) == {"error": "http", "status": 401}
    elif backend == "python":
        with (
            Client(env["FLARE_BASE_URL"], "wrong") as client,
            pytest.raises(HTTPError) as error,
        ):
            client.list_issues()
        assert error.value.status_code == 401
    else:

        async def check():
            async with AsyncClient(env["FLARE_BASE_URL"], "wrong") as client:
                with pytest.raises(HTTPError) as error:
                    await client.list_issues()
                assert error.value.status_code == 401

        asyncio.run(check())
