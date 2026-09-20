import asyncio

import pytest
from flares_client import AsyncClient, Client, ValidationError


@pytest.mark.parametrize("client_type", [Client, AsyncClient])
@pytest.mark.parametrize(
    "kwargs",
    [
        {"base_url": "ftp://example.test"},
        {"base_url": "https://user:secret@example.test"},
        {"base_url": "https://example.test?secret=value"},
        {"base_url": "https://example.test?"},
        {"base_url": "https://example.test#"},
        {"base_url": "https://example.test/prefix?"},
        {"base_url": "https://example.test/prefix#"},
        {"token": "secret\nheader"},
        {"timeout": 0},
        {"timeout": float("nan")},
    ],
)
def test_invalid_configuration(client_type, kwargs):
    with pytest.raises(ValidationError) as error:
        client_type(**{"base_url": "http://127.0.0.1:1", "token": "token", **kwargs})
    assert "secret" not in str(error.value)


@pytest.mark.parametrize("asynchronous", [False, True])
@pytest.mark.parametrize(
    "op,args",
    [
        ("open_issue", {"id": ""}),
        ("open_issue", {"id": "a" * 201}),
        ("open_issue", {"id": "ok", "remind_every_seconds": 0}),
        ("list_issues", {"limit": 1001}),
        ("list_issues", {"offset": -1}),
        ("get_delivery", {"id": 0}),
        ("alert", {"title": "x", "message": "x", "idempotency_key": "secret\n"}),
        ("register_heartbeat", {"id": "x", "title": "x", "interval_seconds": 0}),
    ],
)
def test_invalid_request_without_network(asynchronous, op, args):
    if asynchronous:

        async def check():
            async with AsyncClient("http://127.0.0.1:1", "token") as client:
                with pytest.raises(ValidationError):
                    await getattr(client, op)(**args)

        asyncio.run(check())
    else:
        with Client("http://127.0.0.1:1", "token") as client:
            with pytest.raises(ValidationError):
                getattr(client, op)(**args)
