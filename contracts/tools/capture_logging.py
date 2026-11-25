# Copyright (c) 2023-present Plane Software, Inc. and contributors
# SPDX-License-Identifier: AGPL-3.0-only
# See the LICENSE file for details.

"""Record the request-logging behaviour from Plane's own source, without Django.

The oracle is the *text* of ``apps/api/plane/middleware/logger.py`` and
``apps/api/plane/utils/ip_address.py``: the helpers are pulled out with ``ast``
and executed in a bare namespace, so the answers come from Plane's code rather
than from a re-typed copy. Importing Django is not required and nothing touches
a database or a container.

The JSON log line is produced by the same library Plane pins
(``python-json-logger==4.0.0``) with the format from
``plane/settings/production.py``, so the Rust formatter is compared against the
real encoder. ``logging.Formatter.converter`` is ``time.gmtime`` so ``asctime``
is UTC and the golden does not depend on the machine's timezone.

Run from the repo root:

    apps\\api\\.venv\\Scripts\\python.exe contracts\\tools\\capture_logging.py

The output is ``contracts/golden/logging-cases.json``; re-running it must not
change a byte.
"""

import ast
import json
import logging
import pathlib
import sys
import time

from pythonjsonlogger import json as pjl

REPO = pathlib.Path(__file__).resolve().parents[2]
LOGGER = REPO / "apps" / "api" / "plane" / "middleware" / "logger.py"
IP = REPO / "apps" / "api" / "plane" / "utils" / "ip_address.py"
SETTINGS = REPO / "apps" / "api" / "plane" / "settings"
OUT = REPO / "contracts" / "golden" / "logging-cases.json"

# Python renders ``asctime`` in the process timezone; pin it to UTC so the
# golden is reproducible on any machine.
logging.Formatter.converter = time.gmtime


class _HeaderMap:
    """Minimal stand-in for Django's case-insensitive ``request.headers``."""

    def __init__(self, pairs):
        self._pairs = list(pairs)

    def items(self):
        return list(self._pairs)


class _TokenRequest:
    """Just enough of a request for ``_redacted_headers``."""

    def __init__(self, pairs):
        self.headers = _HeaderMap(pairs)


class _Meta:
    def __init__(self, meta):
        self.META = meta


class _Request:
    def __init__(self, path, method):
        self.path = path
        self.method = method


def eval_node(node, path):
    namespace = {"__builtins__": {"frozenset": frozenset, "int": int, "str": str}}
    return eval(compile(ast.Expression(node), str(path), "eval"), namespace)


def _method(cls, name):
    for node in cls.body:
        if isinstance(node, ast.FunctionDef) and node.name == name:
            return node
    raise SystemExit(f"{cls.name}.{name} not found")


def load_helpers():
    """Execute the logging helpers in a namespace without importing Django."""
    tree = ast.parse(LOGGER.read_text(encoding="utf-8"))
    request_logger = next(
        n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == "RequestLoggerMiddleware"
    )
    token_logger = next(
        n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == "APITokenLogMiddleware"
    )

    body = []
    for node in (
        _method(request_logger, "_should_log_route"),
        _method(token_logger, "_safe_decode_body"),
        _method(token_logger, "_redacted_headers"),
    ):
        # Drop the leading `self` argument so the function can be called directly,
        # and drop the annotations: they name `Request`/`HttpRequest`, which only
        # exist once Django is imported.
        node.args.args = node.args.args[1:]
        for argument in (*node.args.args, *node.args.kwonlyargs):
            argument.annotation = None
        node.returns = None
        body.append(node)

    ip_tree = ast.parse(IP.read_text(encoding="utf-8"))
    body += [n for n in ip_tree.body if isinstance(n, ast.FunctionDef) and n.name == "get_client_ip"]
    if not any(isinstance(n, ast.FunctionDef) and n.name == "get_client_ip" for n in body):
        raise SystemExit("get_client_ip moved or renamed")

    class _Tokens:
        # `_redacted_headers` reads `self.SENSITIVE_HEADERS`; supply it from source.
        SENSITIVE_HEADERS = class_constant(tree, "APITokenLogMiddleware", "SENSITIVE_HEADERS")

    namespace = {
        "logging": logging,
        **module_constants(tree, {"MAX_LOGGED_BODY_LENGTH", "TRUNCATION_MARKER"}),
        "time": time,
        "frozenset": frozenset,
        "len": len,
        "str": str,
        "bytes": bytes,
        "int": int,
        "unicode": str,
        "UnicodeDecodeError": UnicodeDecodeError,
        "__builtins__": {},
    }
    exec(compile(ast.Module(body=body, type_ignores=[]), str(LOGGER), "exec"), namespace)
    namespace["self"] = _Tokens
    return namespace


def module_constants(tree, names):
    out = {}
    for node in tree.body:
        if isinstance(node, ast.Assign) and len(node.targets) == 1:
            target = node.targets[0]
            if isinstance(target, ast.Name) and target.id in names:
                out[target.id] = eval_node(node.value, LOGGER)
    return out


def class_constant(tree, class_name, attribute):
    cls = next(n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == class_name)
    for node in cls.body:
        if isinstance(node, ast.Assign) and len(node.targets) == 1:
            target = node.targets[0]
            if isinstance(target, ast.Name) and target.id == attribute:
                return eval_node(node.value, LOGGER)
    raise SystemExit(f"{class_name}.{attribute} not found")


def _dict_of(node):
    return {k.value: v for k, v in zip(node.keys, node.values) if isinstance(k, ast.Constant)}


def _eval_level(level, path, debug):
    if isinstance(level, ast.Constant):
        return level.value
    namespace = {"DEBUG": debug, "__builtins__": {}}
    return eval(compile(ast.Expression(level), str(path), "eval"), namespace)


def settings_logging(path):
    """Return the json format and ``{logger: {debug, release}}`` from source."""
    tree = ast.parse(path.read_text(encoding="utf-8"))
    assign = next(
        n
        for n in tree.body
        if isinstance(n, ast.Assign)
        and isinstance(n.targets[0], ast.Name)
        and n.targets[0].id == "LOGGING"
    )
    config = _dict_of(assign.value)

    json_fmt = _dict_of(config["formatters"])["json"]
    fmt = next(v.value for k, v in zip(json_fmt.keys, json_fmt.values) if k.value == "fmt")

    resolved = {}
    for name, logger in _dict_of(config["loggers"]).items():
        level = next(v for k, v in zip(logger.keys, logger.values) if k.value == "level")
        resolved[name] = {
            "debug": _eval_level(level, path, True),
            "release": _eval_level(level, path, False),
        }
    return fmt, resolved


def build_line(formatter, spec):
    record = logging.LogRecord(
        spec["logger"],
        getattr(logging, spec["level"]),
        f"/app/{spec['module']}.py",
        1,
        spec["msg"],
        tuple(spec.get("args", ())),
        None,
    )
    record.created = spec["created"]
    record.msecs = (record.created - int(record.created)) * 1000
    record.module = spec["module"]
    for key, value in spec["extra"].items():
        setattr(record, key, value)
    return formatter.format(record)


def main():
    tree = ast.parse(LOGGER.read_text(encoding="utf-8"))
    constants = module_constants(tree, {"MAX_LOGGED_BODY_LENGTH", "TRUNCATION_MARKER"})
    constants["SENSITIVE_HEADERS"] = sorted(
        class_constant(tree, "APITokenLogMiddleware", "SENSITIVE_HEADERS")
    )

    ns = load_helpers()
    safe_decode = ns["_safe_decode_body"]
    redacted = ns["_redacted_headers"]
    should_log = ns["_should_log_route"]
    client_ip = ns["get_client_ip"]

    production_fmt, production = settings_logging(SETTINGS / "production.py")
    local_fmt, local = settings_logging(SETTINGS / "local.py")
    if local_fmt != production_fmt:
        raise SystemExit("local and production use different json formatters")
    formatter = pjl.JsonFormatter(production_fmt)

    line_cases = [
        {
            "name": "request-info",
            "logger": "plane.api.request",
            "level": "INFO",
            "module": "logger",
            "msg": "GET /api/v1/workspaces/ 200",
            "created": 1700000000.123,
            "extra": {
                "path": "/api/v1/workspaces/",
                "method": "GET",
                "status_code": 200,
                "duration_ms": 7,
                "remote_addr": "203.0.113.9",
                "user_agent": "Mozilla/5.0",
                "user_id": None,
            },
        },
        {
            "name": "request-authenticated",
            "logger": "plane.api.request",
            "level": "INFO",
            "module": "logger",
            "msg": "POST /api/v1/issues/ 201",
            "created": 1700000001.004,
            "extra": {
                "path": "/api/v1/issues/",
                "method": "POST",
                "status_code": 201,
                "duration_ms": 0,
                "remote_addr": "198.51.100.7",
                "user_agent": "",
                "user_id": 42,
            },
        },
        {
            "name": "error-level",
            "logger": "plane.exception",
            "level": "ERROR",
            "module": "exception_logger",
            "msg": "boom",
            "created": 1700000002.999,
            "extra": {},
        },
        {
            "name": "non-ascii",
            "logger": "plane.api.request",
            "level": "INFO",
            "module": "logger",
            "msg": "GET /api/v1/projects/Ren\u00e9/ 200",
            "created": 1700000003.5,
            "extra": {"user_agent": "caf\u00e9/1.0", "duration_ms": 12},
        },
        {
            "name": "escaping",
            "logger": "plane.api.request",
            "level": "INFO",
            "module": "logger",
            "msg": "GET /a\"b\\\\c 404",
            "created": 1700000004.25,
            "extra": {"user_agent": "line\nbreak\tand \"quotes\"", "duration_ms": 3},
        },
        {
            "name": "float-and-bool",
            "logger": "plane.api.request",
            "level": "INFO",
            "module": "logger",
            "msg": "slow",
            "created": 1700000005.0,
            "extra": {"duration_ms": 1.5, "user_id": 0, "status_code": 200},
        },
        {
            "name": "args-substitution",
            "logger": "plane.api.request",
            "level": "INFO",
            "module": "logger",
            "msg": "%s %s %s",
            "args": ["GET", "/", 200],
            "created": 1700000006.042,
            "extra": {"method": "GET", "path": "/", "status_code": 200},
        },
    ]
    lines = [
        {"name": case["name"], "record": case, "line": build_line(formatter, case)}
        for case in line_cases
    ]

    route_cases = [
        {"path": "/", "method": "GET"},
        {"path": "/", "method": "POST"},
        {"path": "/", "method": "get"},
        {"path": "/robots.txt", "method": "GET"},
        {"path": "/api/v1/", "method": "GET"},
    ]
    route_cases = [
        dict(case, log=should_log(_Request(case["path"], case["method"]))) for case in route_cases
    ]

    ip_cases = [
        {"xff": "1.2.3.4, 5.6.7.8", "remote": "10.0.0.1"},
        {"xff": " 1.2.3.4 ", "remote": "10.0.0.1"},
        {"xff": "", "remote": "10.0.0.1"},
        {"xff": None, "remote": "10.0.0.1"},
        {"xff": None, "remote": None},
    ]
    for case in ip_cases:
        meta = {}
        if case["remote"]:
            meta["REMOTE_ADDR"] = case["remote"]
        if case["xff"] is not None:
            meta["HTTP_X_FORWARDED_FOR"] = case["xff"]
        case["ip"] = client_ip(_Meta(meta))

    payloads = [
        ("none", None),
        ("empty", b""),
        ("json", b'{"a": 1}'),
        ("png", b"\x89PNG\r\n\x1a\n" + b"0" * 8),
        ("jpeg", b"\xff\xd8\xff\xe0" + b"0" * 8),
        ("pdf", b"%PDF-1.4\n"),
        ("latin1", b"\xff\xfe\x00"),
        ("partial-magic", b"\x89P"),
        ("newline", b"line1\nline2"),
        ("at-limit", b"x" * constants["MAX_LOGGED_BODY_LENGTH"]),
        ("over-limit", b"y" * (constants["MAX_LOGGED_BODY_LENGTH"] + 1)),
        ("unicode-under-limit", "\u00e9".encode() * 6000),
        ("unicode-over-limit", "\u00e9".encode() * 6000 + b"z"),
    ]
    decode_cases = []
    for name, payload in payloads:
        decoded = safe_decode(payload)
        decode_cases.append(
            {
                "name": name,
                "length": None if payload is None else len(payload),
                "decoded": decoded,
                "decoded_length": None if decoded is None else len(decoded),
                "truncated": decoded is not None
                and decoded.endswith(constants["TRUNCATION_MARKER"]),
            }
        )

    header_cases = [
        [("X-Api-Key", "secret"), ("Authorization", "Bearer t"), ("Cookie", "s=1")],
        [("content-type", "application/json"), ("X-Api-Key", "k")],
        [("AUTHORIZATION", "Basic x")],
        [("x-forwarded-for", "1.2.3.4")],
        [],
    ]
    header_cases = [
        {"headers": pairs, "rendered": redacted(_TokenRequest(pairs))} for pairs in header_cases
    ]

    golden = {
        "schema": "plane/logging-cases@1",
        "python": ".".join(str(part) for part in sys.version_info[:3]),
        "source": "apps/api/plane/middleware/logger.py, apps/api/plane/utils/ip_address.py, apps/api/plane/settings",
        "json_formatter": {
            "fmt": production_fmt,
            "package": "python-json-logger==4.0.0",
            "asctime_tz": "UTC",
        },
        "constants": constants,
        "loggers": {"local": local, "production": production},
        "lines": lines,
        "should_log_route": route_cases,
        "client_ip": ip_cases,
        "safe_decode_body": decode_cases,
        "redacted_headers": header_cases,
    }
    OUT.write_text(
        json.dumps(golden, indent=1, ensure_ascii=False) + "\n", encoding="utf-8", newline="\n"
    )
    print(f"wrote {OUT.relative_to(REPO)}")


if __name__ == "__main__":
    main()
