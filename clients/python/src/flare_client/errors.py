"""Sanitized, distinguishable client errors."""


class FlareError(Exception):
    """Base class for Flare client errors."""


class ValidationError(FlareError, ValueError):
    """Invalid client configuration or request arguments."""


class TransportError(FlareError):
    def __init__(self, *, timed_out: bool = False):
        self.timed_out = timed_out
        super().__init__(
            "API request failed or timed out; delivery may have occurred. "
            "Reuse the alert idempotency key or check issue state before retrying"
        )


class HTTPError(FlareError):
    def __init__(self, status_code: int):
        self.status_code = status_code
        super().__init__(f"API returned HTTP {status_code}")


class DecodeError(FlareError):
    def __init__(self):
        super().__init__("API returned an invalid response")
