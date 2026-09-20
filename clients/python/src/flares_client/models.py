"""Typed API models. Response objects accept additive server fields."""

from datetime import datetime
from enum import StrEnum
from typing import Annotated

from pydantic import BaseModel, ConfigDict, Field

IssueId = Annotated[str, Field(strict=True, min_length=1, max_length=200)]
Title = Annotated[str, Field(strict=True, min_length=1, max_length=250)]
Message = Annotated[str, Field(strict=True, min_length=1, max_length=1024)]
Interval = Annotated[int, Field(strict=True, ge=1, le=31_536_000)]


class Severity(StrEnum):
    INFO = "info"
    WARNING = "warning"
    CRITICAL = "critical"


class IssueStatus(StrEnum):
    OPEN = "open"
    CLOSED = "closed"


class NotificationStatus(StrEnum):
    PENDING = "pending"
    SENT = "sent"
    FAILED = "failed"
    UNKNOWN = "unknown"
    NOT_ATTEMPTED = "not_attempted"


class Request(BaseModel):
    model_config = ConfigDict(extra="forbid", hide_input_in_errors=True)


class OpenIssue(Request):
    id: IssueId
    title: Title | None = None
    message: Message | None = None
    severity: Severity = Severity.WARNING
    remind_every_seconds: Interval | None = None
    notify_on_resolution: bool = False


class CloseIssue(Request):
    id: IssueId


class Alert(Request):
    title: Title
    message: Message
    severity: Severity = Severity.WARNING
    group_key: IssueId | None = None


class HeartbeatInput(Request):
    id: IssueId
    title: Title
    interval_seconds: Interval
    grace_seconds: Annotated[int, Field(strict=True, ge=0, le=31_536_000)] = 0
    severity: Severity = Severity.WARNING
    notify_on_recovery: bool = False


class Notification(BaseModel):
    status: NotificationStatus
    error: str | None


class Issue(BaseModel):
    id: str
    status: IssueStatus
    title: str
    message: str
    severity: Severity
    remind_every_seconds: int | None
    notify_on_resolution: bool
    delivery_id: int | None
    created_at: datetime
    updated_at: datetime
    opened_at: datetime
    closed_at: datetime | None
    opening_count: int
    notification: Notification


class MutationResult(BaseModel):
    delivery_id: int | None
    issue: Issue
    changed: bool
    notification: Notification


class AlertResult(BaseModel):
    delivery_id: int
    notification: Notification


class IssueList(BaseModel):
    items: list[Issue]
    total: int
    limit: int
    offset: int


class Heartbeat(BaseModel):
    id: str
    title: str
    interval_seconds: int
    grace_seconds: int
    severity: Severity
    notify_on_recovery: bool
    last_seen: int
    due_at: int
    overdue: bool


class DestinationOutcome(BaseModel):
    destination: str
    attempts: int
    notification: Notification


class Delivery(BaseModel):
    id: int
    title: str
    message: str
    severity: Severity
    kind: str
    count: int
    created_at: int
    next_attempt_at: int
    notification: Notification
    destinations: list[DestinationOutcome]


class Health(BaseModel):
    status: str
