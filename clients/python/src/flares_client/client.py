"""HTTP clients with identical synchronous and asynchronous operations."""

from os import PathLike
from typing import Any, Self

import httpx

from ._common import body, decode, delivery_path, key_header, listing, settings
from ._config import default_path, load_config
from .errors import TransportError
from .models import (
    Alert,
    AlertResult,
    CloseIssue,
    Delivery,
    Health,
    Heartbeat,
    HeartbeatInput,
    Issue,
    IssueList,
    IssueStatus,
    MutationResult,
    OpenIssue,
    Severity,
)


class Client:
    @classmethod
    def from_config(cls, path: str | PathLike[str]) -> Self:
        """Read one YAML file, resolving secret files against its canonical directory."""
        base_url, token, timeout = load_config(path)
        return cls(base_url, token, timeout=timeout)

    @classmethod
    def from_default_config(cls) -> Self:
        """Opt into XDG/HOME, then system client.yaml discovery."""
        return cls.from_config(default_path())

    def __init__(self, base_url: str, token: str, *, timeout: float = 15.0):
        self._base_url, headers = settings(base_url, token, timeout)
        self._client = httpx.Client(headers=headers, timeout=timeout, follow_redirects=False)

    def __enter__(self) -> Self:
        self._client.__enter__()
        return self

    def __exit__(self, *args: Any) -> None:
        self._client.__exit__(*args)

    def close(self) -> None:
        self._client.close()

    def _request(self, method: str, path: str, model: Any, **kwargs: Any) -> Any:
        try:
            response = self._client.request(method, self._base_url + path, **kwargs)
        except httpx.HTTPError as error:
            raise TransportError(timed_out=isinstance(error, httpx.TimeoutException)) from None
        return decode(response, model)

    def open_issue(
        self,
        id: str,
        *,
        title: str | None = None,
        message: str | None = None,
        severity: Severity | str = Severity.WARNING,
        remind_every_seconds: int | None = None,
        notify_on_resolution: bool = False,
    ) -> MutationResult:
        data = body(
            OpenIssue,
            id=id,
            title=title,
            message=message,
            severity=severity,
            remind_every_seconds=remind_every_seconds,
            notify_on_resolution=notify_on_resolution,
        )
        return self._request("POST", "/v1/issues/open", MutationResult, json=data)

    def close_issue(self, id: str) -> MutationResult:
        return self._request(
            "POST", "/v1/issues/close", MutationResult, json=body(CloseIssue, id=id)
        )

    def get_issue(self, id: str) -> Issue:
        return self._request("GET", "/v1/issue", Issue, params=body(CloseIssue, id=id))

    def list_issues(
        self, *, status: IssueStatus | str | None = None, limit: int = 100, offset: int = 0
    ) -> IssueList:
        return self._request("GET", "/v1/issues", IssueList, params=listing(status, limit, offset))

    def alert(
        self,
        title: str,
        message: str,
        *,
        severity: Severity | str = Severity.WARNING,
        group_key: str | None = None,
        idempotency_key: str | None = None,
    ) -> AlertResult:
        return self._request(
            "POST",
            "/v1/alerts",
            AlertResult,
            json=body(Alert, title=title, message=message, severity=severity, group_key=group_key),
            headers=key_header(idempotency_key),
        )

    def get_delivery(self, id: int) -> Delivery:
        return self._request("GET", delivery_path(id), Delivery)

    def register_heartbeat(
        self,
        id: str,
        *,
        title: str,
        interval_seconds: int,
        grace_seconds: int = 0,
        severity: Severity | str = Severity.WARNING,
        notify_on_recovery: bool = False,
    ) -> Heartbeat:
        return self._request(
            "POST",
            "/v1/heartbeats",
            Heartbeat,
            json=body(
                HeartbeatInput,
                id=id,
                title=title,
                interval_seconds=interval_seconds,
                grace_seconds=grace_seconds,
                severity=severity,
                notify_on_recovery=notify_on_recovery,
            ),
        )

    def check_in(self, id: str) -> Heartbeat:
        return self._request(
            "POST", "/v1/heartbeats/check-in", Heartbeat, json=body(CloseIssue, id=id)
        )

    def list_heartbeats(self) -> list[Heartbeat]:
        return self._request("GET", "/v1/heartbeats", list[Heartbeat])

    def delete_heartbeat(self, id: str) -> None:
        return self._request("DELETE", "/v1/heartbeat", None, params=body(CloseIssue, id=id))

    def health(self) -> Health:
        return self._request("GET", "/healthz", Health)

    def readiness(self) -> Health:
        return self._request("GET", "/readyz", Health)

    def metrics(self) -> str:
        return self._request("GET", "/metrics", str)


class AsyncClient:
    @classmethod
    def from_config(cls, path: str | PathLike[str]) -> Self:
        """Read one YAML file, resolving secret files against its canonical directory."""
        base_url, token, timeout = load_config(path)
        return cls(base_url, token, timeout=timeout)

    @classmethod
    def from_default_config(cls) -> Self:
        """Opt into XDG/HOME, then system client.yaml discovery."""
        return cls.from_config(default_path())

    def __init__(self, base_url: str, token: str, *, timeout: float = 15.0):
        self._base_url, headers = settings(base_url, token, timeout)
        self._client = httpx.AsyncClient(headers=headers, timeout=timeout, follow_redirects=False)

    async def __aenter__(self) -> Self:
        await self._client.__aenter__()
        return self

    async def __aexit__(self, *args: Any) -> None:
        await self._client.__aexit__(*args)

    async def aclose(self) -> None:
        await self._client.aclose()

    async def _request(self, method: str, path: str, model: Any, **kwargs: Any) -> Any:
        try:
            response = await self._client.request(method, self._base_url + path, **kwargs)
        except httpx.HTTPError as error:
            raise TransportError(timed_out=isinstance(error, httpx.TimeoutException)) from None
        return decode(response, model)

    async def open_issue(
        self,
        id: str,
        *,
        title: str | None = None,
        message: str | None = None,
        severity: Severity | str = Severity.WARNING,
        remind_every_seconds: int | None = None,
        notify_on_resolution: bool = False,
    ) -> MutationResult:
        data = body(
            OpenIssue,
            id=id,
            title=title,
            message=message,
            severity=severity,
            remind_every_seconds=remind_every_seconds,
            notify_on_resolution=notify_on_resolution,
        )
        return await self._request("POST", "/v1/issues/open", MutationResult, json=data)

    async def close_issue(self, id: str) -> MutationResult:
        return await self._request(
            "POST", "/v1/issues/close", MutationResult, json=body(CloseIssue, id=id)
        )

    async def get_issue(self, id: str) -> Issue:
        return await self._request("GET", "/v1/issue", Issue, params=body(CloseIssue, id=id))

    async def list_issues(
        self, *, status: IssueStatus | str | None = None, limit: int = 100, offset: int = 0
    ) -> IssueList:
        return await self._request(
            "GET", "/v1/issues", IssueList, params=listing(status, limit, offset)
        )

    async def alert(
        self,
        title: str,
        message: str,
        *,
        severity: Severity | str = Severity.WARNING,
        group_key: str | None = None,
        idempotency_key: str | None = None,
    ) -> AlertResult:
        return await self._request(
            "POST",
            "/v1/alerts",
            AlertResult,
            json=body(Alert, title=title, message=message, severity=severity, group_key=group_key),
            headers=key_header(idempotency_key),
        )

    async def get_delivery(self, id: int) -> Delivery:
        return await self._request("GET", delivery_path(id), Delivery)

    async def register_heartbeat(
        self,
        id: str,
        *,
        title: str,
        interval_seconds: int,
        grace_seconds: int = 0,
        severity: Severity | str = Severity.WARNING,
        notify_on_recovery: bool = False,
    ) -> Heartbeat:
        return await self._request(
            "POST",
            "/v1/heartbeats",
            Heartbeat,
            json=body(
                HeartbeatInput,
                id=id,
                title=title,
                interval_seconds=interval_seconds,
                grace_seconds=grace_seconds,
                severity=severity,
                notify_on_recovery=notify_on_recovery,
            ),
        )

    async def check_in(self, id: str) -> Heartbeat:
        return await self._request(
            "POST", "/v1/heartbeats/check-in", Heartbeat, json=body(CloseIssue, id=id)
        )

    async def list_heartbeats(self) -> list[Heartbeat]:
        return await self._request("GET", "/v1/heartbeats", list[Heartbeat])

    async def delete_heartbeat(self, id: str) -> None:
        return await self._request("DELETE", "/v1/heartbeat", None, params=body(CloseIssue, id=id))

    async def health(self) -> Health:
        return await self._request("GET", "/healthz", Health)

    async def readiness(self) -> Health:
        return await self._request("GET", "/readyz", Health)

    async def metrics(self) -> str:
        return await self._request("GET", "/metrics", str)
