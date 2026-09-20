"""Failure semantics tested through public clients and real HTTP connections."""

import asyncio
import http.server
import json
import os
import subprocess
import threading
import time

import pytest
from flares_client import AsyncClient, Client, DecodeError, HTTPError, TransportError
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
            if mode in ("body_timeout", "body_disconnect"):
                self.send_header("Content-Length", "100")
            self.end_headers()
            if mode in ("body_timeout", "body_disconnect"):
                self.wfile.write(b'{"status":')
                self.wfile.flush()
                if mode == "body_timeout":
                    time.sleep(0.2)
                self.close_connection = True
                return
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
                "FLARES_BASE_URL": f"http://127.0.0.1:{server.server_port}",
                "FLARES_API_TOKEN": "secret-token",
                "FLARES_TIMEOUT": "0.05",
            },
        )
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


@pytest.mark.parametrize("backend", ["rust", "python", "python_async", "bash"])
@pytest.mark.parametrize(
    "op,mode,kind,status",
    [
        ("health", "malformed", "decode", None),
        ("health", "shape", "decode", None),
        ("health", "redirect", "http", 302),
        ("health", "http", "http", 503),
        ("health", "disconnect", "transport", None),
        ("health", "timeout", "transport", None),
        ("health", "body_disconnect", "transport", None),
        ("health", "body_timeout", "transport", None),
        ("metrics", "body_disconnect", "transport", None),
        ("metrics", "body_timeout", "transport", None),
    ],
)
def test_failure_does_not_retry_or_leak_secrets(responder, backend, op, mode, kind, status):
    state, env = responder
    state["mode"] = mode
    if backend == "bash":
        output = shell(env, op, {})
        assert output.returncode == 1
        assert not output.stdout
        assert "secret" not in output.stderr
        if status:
            assert str(status) in output.stderr
    elif backend == "rust":
        output = subprocess.run(
            [str(ROOT / "target/debug/examples/contract_driver")],
            input=json.dumps({"op": op, "args": {}}),
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
                assert error.value.timed_out == (mode in ("timeout", "body_timeout"))

        if backend == "python":
            with Client(env["FLARES_BASE_URL"], env["FLARES_API_TOKEN"], timeout=0.05) as client:
                with pytest.raises(error_type) as error:
                    getattr(client, op)()
                check_error(error)
        else:

            async def check():
                async with AsyncClient(
                    env["FLARES_BASE_URL"], env["FLARES_API_TOKEN"], timeout=0.05
                ) as client:
                    with pytest.raises(error_type) as error:
                        await getattr(client, op)()
                    check_error(error)

            asyncio.run(check())
    assert state["requests"] == 1
