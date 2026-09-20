import math
from typing import Any

import httpx
from pydantic import BaseModel, TypeAdapter
from pydantic import ValidationError as ModelError

from .errors import DecodeError, HTTPError, ValidationError


def settings(base_url: str, token: str, timeout: float) -> tuple[str, dict[str, str]]:
    try:
        url = httpx.URL(base_url)
    except (httpx.InvalidURL, TypeError):
        raise ValidationError("Invalid API base URL") from None
    if (
        url.scheme not in ("http", "https")
        or not url.host
        or url.username
        or url.password
        # HTTPX retains empty delimiters even when query/fragment values are empty.
        or "?" in str(url)
        or "#" in str(url)
    ):
        raise ValidationError(
            "API base URL must be HTTP(S) without credentials, query, or fragment"
        )
    if not isinstance(token, str) or not token or not all(33 <= ord(c) <= 126 for c in token):
        raise ValidationError("API token must be nonempty printable ASCII without spaces")
    if (
        not isinstance(timeout, (float, int))
        or isinstance(timeout, bool)
        or not math.isfinite(timeout)
        or not 0 < timeout <= 86400
    ):
        raise ValidationError("Timeout must be greater than zero and at most 86400 seconds")
    return str(url).rstrip("/"), {"Authorization": f"Bearer {token}"}


def body(model: type[BaseModel], **values: Any) -> dict[str, Any]:
    try:
        return model.model_validate(values).model_dump(mode="json")
    except ModelError as error:
        fields = sorted({str(e["loc"][0]) for e in error.errors() if e["loc"]})
        raise ValidationError(f"Invalid request fields: {', '.join(fields)}") from None


def key_header(key: str | None) -> dict[str, str]:
    if key is None:
        return {}
    if (
        not isinstance(key, str)
        or not 1 <= len(key) <= 200
        or not all(33 <= ord(c) <= 126 for c in key)
    ):
        raise ValidationError("Invalid idempotency key")
    return {"Idempotency-Key": key}


def listing(status: str | None, limit: int, offset: int) -> dict[str, Any]:
    if (
        status not in (None, "open", "closed")
        or type(limit) is not int
        or not 1 <= limit <= 1000
        or type(offset) is not int
        or not 0 <= offset <= 4_294_967_295
    ):
        raise ValidationError("Invalid status, limit, or offset")
    params = {"limit": limit, "offset": offset}
    if status is not None:
        params["status"] = status
    return params


def delivery_path(id: int) -> str:
    if type(id) is not int or not 1 <= id <= 9_223_372_036_854_775_807:
        raise ValidationError("delivery id must be a positive 64-bit integer")
    return f"/v1/deliveries/{id}"


def decode(response: httpx.Response, model: Any) -> Any:
    if not response.is_success:
        raise HTTPError(response.status_code)
    if model is str:
        return response.text
    try:
        value = response.json()
        if model is None:
            if not isinstance(value, dict) or value.get("deleted") is not True:
                raise ValueError
            return None
        return TypeAdapter(model).validate_python(value)
    except (ValueError, TypeError):
        raise DecodeError() from None
