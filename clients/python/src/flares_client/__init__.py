"""Flares client library. HTTP success and notification delivery are separate outcomes."""

from .client import AsyncClient, Client
from .errors import DecodeError, FlaresError, HTTPError, TransportError, ValidationError
from .models import (
    Alert,
    AlertResult,
    Delivery,
    DestinationOutcome,
    Health,
    Heartbeat,
    HeartbeatInput,
    Issue,
    IssueList,
    IssueStatus,
    MutationResult,
    Notification,
    NotificationStatus,
    OpenIssue,
    Severity,
)

__version__ = "0.1.1"
__all__ = [
    "Client",
    "AsyncClient",
    "FlaresError",
    "ValidationError",
    "TransportError",
    "HTTPError",
    "DecodeError",
    "Alert",
    "AlertResult",
    "Delivery",
    "DestinationOutcome",
    "Health",
    "Heartbeat",
    "HeartbeatInput",
    "Issue",
    "IssueList",
    "IssueStatus",
    "MutationResult",
    "Notification",
    "NotificationStatus",
    "OpenIssue",
    "Severity",
]
