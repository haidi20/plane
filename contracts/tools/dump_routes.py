# Copyright (c) 2023-present Plane Software, Inc. and contributors
# SPDX-License-Identifier: AGPL-3.0-only
# See the LICENSE file for details.

"""Dump the live Django URL resolver into contracts/routes.json and endpoints.csv.

Run inside the Django container after Django is healthy:

    docker cp contracts/tools/dump_routes.py community-api-1:/tmp/dump_routes.py
    docker exec -e ROUTE_OUT=/tmp/routes.json -e ROUTE_CSV=/tmp/endpoints.csv \
      community-api-1 python manage.py shell -c "exec(open('/tmp/dump_routes.py').read())"

Environment matrix: run once per value, in a fresh process:

    ENABLE_DRF_SPECTACULAR=0 -> contracts/routes.json            (production surface)
    ENABLE_DRF_SPECTACULAR=1 -> contracts/routes-spectacular.json (+ /api/schema/*)

Output is deterministic: no timestamp is written, keys are sorted, and routes stay
in resolver order. Re-running against the same Django build must produce
byte-identical files, otherwise route-parity diffs are noise.

Route fields:

    path          Django-style template for path() entries. For re_path() entries
                  this is the mounted prefix plus the raw regex (no template).
    regex         Local regex of a re_path() entry ("" for path() entries).
    full_regex    Concatenated matcher for the whole chain, anchors included.
    is_regex_path true when the entry was registered with re_path().
"""

from __future__ import annotations

import csv
import json
import os
import re
from pathlib import Path

import django
from django.conf import settings
from django.urls import URLPattern, URLResolver, get_resolver
from django.views import View

PARAM_RE = re.compile(r"<(?:([^>:]+):)?([^>]+)>")
NAMED_GROUP_RE = re.compile(r"\(\?P<([^>]+)>")
TRIM_ANCHOR_RE = re.compile(r"(?:\\Z|\$)$")
HTTP_METHODS = ("get", "post", "put", "patch", "delete")
DOORS = (
    ("api/v1/", "api/v1"),
    ("api/public/", "api/public"),
    ("api/instances/", "api/instances"),
    ("api/", "api"),
    ("auth/", "auth"),
)


def _pattern_source(pattern) -> tuple[str | None, str | None]:
    """Return (route, regex_source) for a Django URL pattern."""
    route = getattr(pattern, "_route", None)
    if route is not None:
        return route, None
    regex = getattr(pattern, "regex", None)
    return None, regex.pattern if regex is not None else str(pattern)


def _local_regex(pattern) -> str:
    regex = getattr(pattern, "regex", None)
    return regex.pattern if regex is not None else ""


def _join_regex(prefix: str, part: str, *, terminal: bool) -> str:
    fragment = part
    if prefix:
        fragment = fragment.lstrip("^")
    if not terminal:
        fragment = TRIM_ANCHOR_RE.sub("", fragment)
    return prefix + fragment


def _named_groups(source: str) -> dict[str, str]:
    """Extract `(?P<name>...)` sub-patterns, nesting aware."""
    groups: dict[str, str] = {}
    for match in NAMED_GROUP_RE.finditer(source):
        depth = 1
        index = match.end()
        while index < len(source) and depth:
            char = source[index]
            if char == "\\":
                index += 2
                continue
            if char == "(":
                depth += 1
            elif char == ")":
                depth -= 1
                if depth == 0:
                    break
            index += 1
        groups[match.group(1)] = source[match.end() : index]
    return groups


def _params(route: str | None, pattern) -> list[dict[str, str]]:
    if route is None:
        regex = getattr(pattern, "regex", None)
        if regex is None:
            return []
        groups = _named_groups(regex.pattern)
        return [
            {"name": name, "converter": "regex", "regex": groups.get(name, "?")}
            for name in sorted(regex.groupindex)
        ]

    converters = getattr(pattern, "converters", {}) or {}
    params: list[dict[str, str]] = []
    for match in PARAM_RE.finditer(route):
        converter = match.group(1) or "str"
        config = converters.get(match.group(2))
        params.append(
            {
                "name": match.group(2),
                "converter": converter,
                "regex": getattr(config, "regex", ""),
            }
        )
    return params


def _methods(callback) -> tuple[list[str], str]:
    actions = getattr(callback, "actions", None)
    if actions:
        return sorted({method.upper() for method in actions}), "viewset-actions"

    view_cls = getattr(callback, "cls", None) or getattr(callback, "view_class", None)
    if view_cls is None:
        # Django calls function views for every HTTP method; the function itself
        # decides what to do. This is verified against the live endpoint.
        return ["*"], "django-function-view"

    not_allowed = View.http_method_not_allowed
    found = []
    for method in HTTP_METHODS:
        handler = getattr(view_cls, method, None)
        if handler is not None and handler is not not_allowed:
            found.append(method.upper())
    return found, "class-handlers"


def _view_name(callback) -> str:
    view_cls = getattr(callback, "cls", None) or getattr(callback, "view_class", None)
    target = view_cls or callback
    module = getattr(target, "__module__", "")
    qualname = getattr(target, "__qualname__", getattr(target, "__name__", repr(target)))
    return f"{module}.{qualname}" if module else qualname


def _door(path: str) -> str:
    for prefix, name in DOORS:
        if path.startswith(prefix):
            return name
    return "web"


def walk(
    resolver,
    prefix_path: str = "",
    regex_prefix: str = "",
    namespace: tuple[str, ...] = (),
):
    for entry in resolver.url_patterns:
        if isinstance(entry, URLResolver):
            route, regex = _pattern_source(entry.pattern)
            part = route if route is not None else (regex or "")
            child_namespace = namespace
            if entry.namespace:
                child_namespace = namespace + (entry.namespace,)
            yield from walk(
                entry,
                prefix_path + part,
                _join_regex(regex_prefix, _local_regex(entry.pattern), terminal=False),
                child_namespace,
            )
            continue

        if not isinstance(entry, URLPattern):
            continue

        route, local_regex = _pattern_source(entry.pattern)
        is_regex_path = route is None
        methods, method_source = _methods(entry.callback)
        # re_path() entries have no Django-style template. Keep the mounted
        # prefix plus the raw regex in `path` so the entry stays unique and
        # diffable; `regex` and `full_regex` carry the authoritative matcher.
        segment = route if route is not None else (local_regex or "")
        yield {
            "path": prefix_path + segment,
            "regex": _local_regex(entry.pattern) if is_regex_path else "",
            "full_regex": _join_regex(
                regex_prefix, _local_regex(entry.pattern), terminal=True
            ),
            "is_regex_path": is_regex_path,
            "name": entry.name,
            "namespace": list(namespace),
            "view": _view_name(entry.callback),
            "methods": methods,
            "method_source": method_source,
            "params": _params(route, entry.pattern),
            "owner": "django",
        }


def _duplicates(routes: list[dict]) -> list[dict]:
    seen: dict[tuple[str, str, str], list[int]] = {}
    for route in routes:
        for method in route["methods"] or ["UNKNOWN"]:
            key = (method, route["path"], route["regex"])
            seen.setdefault(key, []).append(route["order"])
    return [
        {
            "method": key[0],
            "path": key[1],
            "regex": key[2],
            "count": len(orders),
            "orders": orders,
        }
        for key, orders in sorted(seen.items())
        if len(orders) > 1
    ]


def main() -> None:
    repo_root = Path(os.environ.get("ROUTE_REPO_ROOT", "/tmp"))
    routes_out = Path(os.environ.get("ROUTE_OUT", repo_root / "routes.json"))
    csv_out = Path(os.environ.get("ROUTE_CSV", repo_root / "endpoints.csv"))

    routes = []
    for order, route in enumerate(walk(get_resolver()), start=1):
        route["order"] = order
        routes.append(route)

    doors: dict[str, dict[str, int]] = {}
    for route in routes:
        bucket = doors.setdefault(_door(route["path"]), {"routes": 0, "endpoints": 0})
        bucket["routes"] += 1
        bucket["endpoints"] += max(len(route["methods"]), 1)

    endpoint_count = sum(max(len(route["methods"]), 1) for route in routes)

    payload = {
        "schema_version": 2,
        "generated_by": "contracts/tools/dump_routes.py",
        "source": "django.urls.get_resolver",
        "django_version": django.get_version(),
        "settings_module": os.environ.get("DJANGO_SETTINGS_MODULE", ""),
        "env": {
            "debug": bool(settings.DEBUG),
            "enable_drf_spectacular": bool(
                getattr(settings, "ENABLE_DRF_SPECTACULAR", False)
            ),
        },
        "route_count": len(routes),
        "endpoint_count": endpoint_count,
        "unknown_method_routes": sum(1 for route in routes if not route["methods"]),
        "wildcard_method_routes": sum(
            1 for route in routes if route["methods"] == ["*"]
        ),
        "regex_route_count": sum(1 for route in routes if route["is_regex_path"]),
        "doors": dict(sorted(doors.items())),
        "duplicates": _duplicates(routes),
        "routes": routes,
    }

    routes_out.parent.mkdir(parents=True, exist_ok=True)
    routes_out.write_text(
        json.dumps(payload, indent=2, ensure_ascii=False, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="",
    )

    csv_out.parent.mkdir(parents=True, exist_ok=True)
    with csv_out.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.writer(handle, lineterminator="\n")
        writer.writerow(
            [
                "order",
                "method",
                "path",
                "name",
                "namespace",
                "view",
                "params",
                "owner",
                "is_regex_path",
                "regex",
                "full_regex",
            ]
        )
        for route in routes:
            for method in route["methods"] or ["UNKNOWN"]:
                writer.writerow(
                    [
                        route["order"],
                        method,
                        route["path"],
                        route["name"] or "",
                        ".".join(route["namespace"]),
                        route["view"],
                        ",".join(param["name"] for param in route["params"]),
                        route["owner"],
                        str(route["is_regex_path"]).lower(),
                        route["regex"],
                        route["full_regex"],
                    ]
                )

    print(
        json.dumps(
            {
                "routes": payload["route_count"],
                "endpoints": payload["endpoint_count"],
                "regex_routes": payload["regex_route_count"],
                "duplicates": len(payload["duplicates"]),
                "spectacular": payload["env"]["enable_drf_spectacular"],
                "routes_json": str(routes_out),
                "endpoints_csv": str(csv_out),
            },
            sort_keys=True,
        )
    )


main()
