"""Sanitized, distinguishable client errors."""


class FlaresError(Exception):
    """Base class for Flares client errors."""


class ValidationError(FlaresError, ValueError):
    """Invalid client configuration or request arguments."""


class TransportError(FlaresError):
    def __init__(self, *, timed_out: bool = False):
        self.timed_out = timed_out
        super().__init__(
            "API request failed or timed out; delivery may have occurred. "
            "Reuse the alert idempotency key or check issue state before retrying"
        )


class HTTPError(FlaresError):
    def __init__(self, status_code: int):
        self.status_code = status_code
        super().__init__(f"API returned HTTP {status_code}")


class DecodeError(FlaresError):
    def __init__(self):
        super().__init__("API returned an invalid response")
