"""Failure semantics tested through public clients and real HTTP connections."""

import asyncio
import http.server
import json
import os
import subprocess
import threading
import time

import pytest
from flare_client import AsyncClient, Client, DecodeError, HTTPError, TransportError
from test_clients import ROOT, shell


@pytest.fixture
def responder():
    state = {"requests": 0, "mode": "malformed"}

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            state["requests"] += 1
            mode = state["mode"]
            if mode == "timeout":
                time.sleep(0.2)
            if mode == "disconnect":
                self.close_connection = True
                return
            code = 302 if mode == "redirect" else 503 if mode == "http" else 200
            self.send_response(code)
            if mode == "redirect":
                self.send_header("Location", "/should-not-be-requested")
            self.end_headers()
            payload = b'{"unexpected":"secret"}' if mode == "shape" else b"secret-not-json"
            try:
                self.wfile.write(payload)
            except (BrokenPipeError, ConnectionResetError):
                pass

        def log_message(self, *args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield (
            state,
            {
                **os.environ,
                "FLARE_BASE_URL": f"http://127.0.0.1:{server.server_port}",
                "FLARE_API_TOKEN": "secret-token",
                "FLARE_TIMEOUT": "0.05",
            },
        )
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


@pytest.mark.parametrize("backend", ["rust", "python", "python_async", "bash"])
@pytest.mark.parametrize(
    "mode,kind,status",
    [
        ("malformed", "decode", None),
        ("shape", "decode", None),
        ("redirect", "http", 302),
        ("http", "http", 503),
        ("disconnect", "transport", None),
        ("timeout", "transport", None),
    ],
)
def test_failure_does_not_retry_or_leak_secrets(responder, backend, mode, kind, status):
    state, env = responder
    state["mode"] = mode
    if backend == "bash":
        output = shell(env, "health", {})
        assert output.returncode == 1
        assert not output.stdout
        assert "secret" not in output.stderr
        if status:
            assert str(status) in output.stderr
    elif backend == "rust":
        output = subprocess.run(
            [str(ROOT / "target/debug/examples/contract_driver")],
            input='{"op":"health","args":{}}',
            env=env,
            capture_output=True,
            text=True,
            check=True,
            timeout=10,
        )
        expected = {"error": kind}
        if status:
            expected["status"] = status
        assert json.loads(output.stdout) == expected
        assert "secret" not in output.stderr
    else:
        error_type = {
            "decode": DecodeError,
            "http": HTTPError,
            "transport": TransportError,
        }[kind]

        def check_error(error):
            assert "secret" not in str(error.value)
            if status:
                assert error.value.status_code == status
            if kind == "transport":
                assert error.value.timed_out == (mode == "timeout")

        if backend == "python":
            with Client(env["FLARE_BASE_URL"], env["FLARE_API_TOKEN"], timeout=0.05) as client:
                with pytest.raises(error_type) as error:
                    client.health()
                check_error(error)
        else:

            async def check():
                async with AsyncClient(
                    env["FLARE_BASE_URL"], env["FLARE_API_TOKEN"], timeout=0.05
                ) as client:
                    with pytest.raises(error_type) as error:
                        await client.health()
                    check_error(error)

            asyncio.run(check())
    assert state["requests"] == 1
