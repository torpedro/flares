"""Explicit file configuration; ordinary constructors never read configuration files."""

import math
import os
import re
from pathlib import Path

import yaml
from yaml.nodes import MappingNode, Node, ScalarNode

from ._common import settings
from .errors import ValidationError


class _Loader(yaml.SafeLoader):
    def resolve(self, kind, value, implicit):
        if kind is not ScalarNode or not implicit[0]:
            return super().resolve(kind, value, implicit)
        # Match serde_yaml_ng's YAML 1.2 scalar interpretation. In particular,
        # yes/on, dates, 0123, 1_000, and 1:20 are strings, while 1e3 is numeric.
        tag = "str"
        if value in ("", "~", "null", "Null", "NULL"):
            tag = "null"
        elif value in ("true", "True", "TRUE", "false", "False", "FALSE"):
            tag = "bool"
        elif not re.fullmatch(r"[+-]?0[0-9]+", value):
            for pattern, base in (
                (r"[+-]?0x[0-9a-fA-F]+", 16),
                (r"[+-]?0o[0-7]+", 8),
                (r"[+-]?0b[01]+", 2),
            ):
                if re.fullmatch(pattern, value):
                    try:
                        number = int(value, base)
                        if -(2**127) <= number < 2**128:
                            tag = "int"
                    except ValueError:
                        pass
                    break
            else:
                if value in (
                    ".inf",
                    ".Inf",
                    ".INF",
                    "+.inf",
                    "+.Inf",
                    "+.INF",
                    "-.inf",
                    "-.Inf",
                    "-.INF",
                    ".nan",
                    ".NaN",
                    ".NAN",
                ):
                    tag = "float"
                elif re.fullmatch(
                    r"[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?", value
                ):
                    if math.isfinite(float(value)):
                        tag = "float"
        return f"tag:yaml.org,2002:{tag}"


def _find_config(xdg: str | None, home: str | None, system: Path) -> Path:
    base = Path(xdg) if xdg and Path(xdg).is_absolute() else None
    if base is None and home:
        base = Path(home) / ".config"
    paths = ([base / "flares/client.yaml"] if base is not None else []) + [system / "client.yaml"]
    for path in paths:
        try:
            # Select broken symlinks too: loading them must fail rather than fall back.
            path.lstat()
        except FileNotFoundError:
            continue
        except OSError:
            raise ValidationError(
                f"Cannot access configuration {path}; supply an explicit configuration path"
            ) from None
        return path
    raise ValidationError(
        f"No client.yaml found; searched {', '.join(map(str, paths))}; "
        "supply an explicit configuration path"
    )


def default_path() -> Path:
    return _find_config(
        os.environ.get("XDG_CONFIG_HOME"), os.environ.get("HOME"), Path("/etc/flares")
    )


def _invalid(field: str, node: Node | None, reason: str) -> ValidationError:
    location = ""
    if node is not None:
        location = f" at line {node.start_mark.line + 1}, column {node.start_mark.column + 1}"
    return ValidationError(f"{field}{location}: {reason}")


def _mapping(node: Node | None, field: str, allowed: set[str]) -> dict[str, Node]:
    if not isinstance(node, MappingNode) or node.tag != "tag:yaml.org,2002:map":
        raise _invalid(field, node, "expected a mapping")
    result = {}
    for key, value in node.value:
        if not isinstance(key, ScalarNode) or key.value not in allowed:
            raise _invalid(field, key, "unknown configuration field")
        if key.value in result:
            raise _invalid(field, key, "duplicate configuration field")
        result[key.value] = value
    return result


def _string(node: Node, field: str, *, literal: bool = False) -> str:
    tags = {"tag:yaml.org,2002:str"}
    if not literal:
        tags |= {f"tag:yaml.org,2002:{kind}" for kind in ("int", "float", "bool")}
    if not isinstance(node, ScalarNode) or node.tag not in tags:
        raise _invalid(field, node, "expected a string")
    return node.value


def _secret(node: Node, directory: Path) -> str:
    if isinstance(node, ScalarNode):
        return _string(node, "api_token", literal=True)
    reference = _mapping(node, "api_token", {"env", "file"})
    if len(reference) != 1:
        raise _invalid("api_token", node, "expected exactly one env or file reference")
    kind, value = next(iter(reference.items()))
    name = _string(value, "api_token", literal=True)
    if kind == "env":
        try:
            token = os.environ[name]
            token.encode("utf-8")
            return token
        except (KeyError, ValueError, UnicodeError):
            raise _invalid(
                "api_token", node, "environment variable is missing or not UTF-8"
            ) from None
    try:
        return (directory / name).read_bytes().decode("utf-8").rstrip("\r\n")
    except (OSError, ValueError, UnicodeError):
        raise _invalid("api_token", node, "cannot read secret file as UTF-8") from None


def _load(path: Path) -> tuple[str, str, float]:
    try:
        text = path.read_bytes().decode("utf-8")
    except (OSError, ValueError, UnicodeError):
        raise ValidationError("configuration: cannot read UTF-8 YAML file") from None
    try:
        node = yaml.compose(text, Loader=_Loader)
    except (yaml.YAMLError, RecursionError) as error:
        mark = getattr(error, "problem_mark", None)
        location = f" at line {mark.line + 1}, column {mark.column + 1}" if mark else ""
        raise ValidationError(f"configuration{location}: invalid configuration YAML") from None
    fields = _mapping(node, "configuration", {"base_url", "api_token", "timeout"})
    if "api_token" not in fields:
        raise _invalid("api_token", node, "required field is missing")
    try:
        directory = path.resolve(strict=True).parent
    except (OSError, RuntimeError, ValueError):
        raise ValidationError("configuration: cannot resolve file directory") from None
    token = _secret(fields["api_token"], directory)
    if not token or not all(33 <= ord(c) <= 126 for c in token):
        raise _invalid(
            "api_token", fields["api_token"], "must be nonempty printable ASCII without whitespace"
        )
    base_url = "http://127.0.0.1:8000"
    url_node = fields.get("base_url")
    if url_node is not None and url_node.tag != "tag:yaml.org,2002:null":
        base_url = _string(url_node, "base_url")
    timeout = 15
    timeout_node = fields.get("timeout")
    if timeout_node is not None and timeout_node.tag != "tag:yaml.org,2002:null":
        duration = _string(timeout_node, "timeout")
        match = re.fullmatch(r"([0-9]+)([smhdw])", duration)
        if not match:
            raise _invalid(
                "timeout",
                timeout_node,
                "expected a whole-second duration with s, m, h, d, or w suffix",
            )
        number = match[1].lstrip("0") or "0"
        if len(number) > 20:
            raise _invalid("timeout", timeout_node, "duration exceeds supported range")
        timeout = int(number) * {"s": 1, "m": 60, "h": 3600, "d": 86400, "w": 604800}[match[2]]
        if not 0 < timeout <= 86400:
            raise _invalid("timeout", timeout_node, "must be greater than 0s and at most 1d")
    try:
        settings(base_url, token, timeout)
    except ValidationError:
        raise _invalid(
            "base_url", url_node, "must be HTTP(S) without credentials, query, or fragment"
        ) from None
    return base_url, token, float(timeout)


def load_config(path: str | os.PathLike[str]) -> tuple[str, str, float]:
    selected = Path(path)
    try:
        return _load(selected)
    except ValidationError as error:
        raise ValidationError(f"{selected}: {error}") from None
