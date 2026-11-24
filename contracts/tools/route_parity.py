# Copyright (c) 2023-present Plane Software, Inc. and contributors
# SPDX-License-Identifier: AGPL-3.0-only
# See the LICENSE file for details.

"""Compare the frozen Django route surface with the Rust route manifest.

The Django side is always `contracts/routes.json`, produced by
`contracts/tools/dump_routes.py` from `django.urls.get_resolver()`. The Rust side
is a manifest emitted by `cargo run -p plane-api --bin route-manifest`, which
walks the axum router the same way Django walks its resolver.

Usage:

    python contracts/tools/route_parity.py \
        --django contracts/routes.json \
        --rust apps/api-rs/contracts/routes.rust.json

Exit codes: 0 = green, 1 = differences found, 2 = bad input.

Checks (all two-way):

* endpoint set `(method, path)` - nothing missing, nothing extra;
* path shape - same literal segments, same converter per parameter position;
* method set per path - no extra or missing verbs;
* duplicates - every `(method, path)` owned exactly once on both sides;
* trailing slash - `/x/` and `/x` are distinct contracts, never merged;
* order - optional (`--check-order`), compared on shared paths.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import defaultdict
from pathlib import Path

PARAM_RE = re.compile(r"<(?:([^>:]+):)?([^>]+)>")
TRAILING_SLASH_RE = re.compile(r"/+$")


def _load(path: Path) -> dict:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        raise SystemExit(f"route_parity: missing file {path}") from None
    except json.JSONDecodeError as exc:
        raise SystemExit(f"route_parity: invalid JSON in {path}: {exc}") from None


def _django_routes(payload: dict) -> list[dict]:
    routes = []
    for route in payload.get("routes", []):
        methods = route.get("methods") or ["UNKNOWN"]
        for method in methods:
            routes.append(
                {
                    "method": method,
                    "path": route["path"],
                    "name": route.get("name"),
                    "order": route.get("order"),
                    "implemented": True,
                }
            )
    return routes


def _rust_routes(payload: dict) -> list[dict]:
    routes = []
    for route in payload.get("routes", []):
        path = route.get("path")
        if path is None:
            raise SystemExit("route_parity: rust route without `path`")
        for method in route.get("methods") or ["UNKNOWN"]:
            routes.append(
                {
                    "method": method,
                    "path": path,
                    "name": route.get("name"),
                    "order": route.get("order"),
                    "implemented": bool(route.get("implemented", True)),
                }
            )
    return routes


def literal_shape(path: str) -> tuple[str, ...]:
    """Path with every parameter segment masked, so only literals remain."""
    return tuple(
        "*" if PARAM_RE.fullmatch(segment) else segment for segment in path.split("/")
    )


def converters(path: str) -> tuple[tuple[str, str], ...]:
    """(name, converter) pairs, in path order."""
    return tuple(
        (match.group(2), match.group(1) or "str") for match in PARAM_RE.finditer(path)
    )


def _key(route: dict) -> tuple[str, str]:
    return route["method"], route["path"]


def _index(routes: list[dict]) -> dict[tuple[str, str], dict]:
    index: dict[tuple[str, str], dict] = {}
    for route in routes:
        index.setdefault(_key(route), route)
    return index


def _duplicates(routes: list[dict]) -> dict[tuple[str, str], int]:
    counts: dict[tuple[str, str], int] = defaultdict(int)
    for route in routes:
        counts[_key(route)] += 1
    return {key: count for key, count in counts.items() if count > 1}


def _trailing_slash_collisions(routes: list[dict]) -> list[dict]:
    by_method: dict[str, set[str]] = defaultdict(set)
    for route in routes:
        by_method[route["method"]].add(route["path"])
    collisions = []
    for method, paths in by_method.items():
        for path in paths:
            stripped = TRAILING_SLASH_RE.sub("", path)
            if stripped in paths and stripped != path:
                collisions.append({"method": method, "without_slash": stripped})
    return sorted(collisions, key=lambda row: (row["method"], row["without_slash"]))


def compare(
    django: list[dict], rust: list[dict], check_order: bool = False
) -> dict:
    django_index = _index(django)
    rust_index = _index(rust)

    missing = sorted(
        (key for key in django_index.keys() - rust_index.keys()),
        key=lambda key: (key[1], key[0]),
    )
    extra = sorted(
        (key for key in rust_index.keys() - django_index.keys()),
        key=lambda key: (key[1], key[0]),
    )

    # Same literals but different `(name, converter)` pairs. A uuid->str swap or
    # a renamed `pk` shows up here instead of hiding inside missing/extra.
    rust_by_shape: dict[tuple[str, ...], set[str]] = defaultdict(set)
    for route in rust:
        rust_by_shape[literal_shape(route["path"])].add(route["path"])
    rust_paths = {route["path"] for route in rust}
    param_mismatches = []
    for route in django:
        if route["path"] in rust_paths:
            continue
        candidates = rust_by_shape.get(literal_shape(route["path"]), set())
        for candidate in sorted(candidates):
            param_mismatches.append(
                {
                    "method": route["method"],
                    "django_path": route["path"],
                    "rust_path": candidate,
                    "django_params": list(converters(route["path"])),
                    "rust_params": list(converters(candidate)),
                }
            )

    method_mismatches = []
    for path in sorted({route["path"] for route in django} & {route["path"] for route in rust}):
        django_methods = sorted(r["method"] for r in django if r["path"] == path)
        rust_methods = sorted(r["method"] for r in rust if r["path"] == path)
        if django_methods != rust_methods:
            method_mismatches.append(
                {
                    "path": path,
                    "django_methods": django_methods,
                    "rust_methods": rust_methods,
                }
            )

    order_mismatches = []
    if check_order:
        for path in sorted({r["path"] for r in django} & {r["path"] for r in rust}):
            django_order = min(r["order"] for r in django if r["path"] == path)
            rust_order = min(r["order"] for r in rust if r["path"] == path)
            if django_order != rust_order:
                order_mismatches.append(
                    {"path": path, "django_order": django_order, "rust_order": rust_order}
                )

    implemented = sum(1 for route in rust if route["implemented"])
    return {
        "django_endpoints": len(django),
        "rust_endpoints": len(rust),
        "rust_implemented": implemented,
        "rust_declared_only": len(rust) - implemented,
        "missing": [{"method": method, "path": path} for method, path in missing],
        "extra": [{"method": method, "path": path} for method, path in extra],
        "param_mismatches": param_mismatches,
        "method_mismatches": method_mismatches,
        "order_mismatches": order_mismatches,
        "duplicate_django": [
            {"method": key[0], "path": key[1], "count": count}
            for key, count in sorted(_duplicates(django).items())
        ],
        "duplicate_rust": [
            {"method": key[0], "path": key[1], "count": count}
            for key, count in sorted(_duplicates(rust).items())
        ],
        "trailing_slash_collisions": {
            "django": _trailing_slash_collisions(django),
            "rust": _trailing_slash_collisions(rust),
        },
    }


def summarize(report: dict) -> str:
    lines = [
        f"django endpoints: {report['django_endpoints']}",
        f"rust endpoints:   {report['rust_endpoints']}"
        f" (implemented {report['rust_implemented']},"
        f" declared-only {report['rust_declared_only']})",
        f"missing: {len(report['missing'])}",
        f"extra: {len(report['extra'])}",
        f"param mismatches: {len(report['param_mismatches'])}",
        f"method mismatches: {len(report['method_mismatches'])}",
        f"order mismatches: {len(report['order_mismatches'])}",
        f"duplicates: django {len(report['duplicate_django'])},"
        f" rust {len(report['duplicate_rust'])}",
        f"trailing-slash collisions:"
        f" django {len(report['trailing_slash_collisions']['django'])},"
        f" rust {len(report['trailing_slash_collisions']['rust'])}",
    ]
    for label, key in (
        ("missing", "missing"),
        ("extra", "extra"),
        ("param", "param_mismatches"),
        ("method", "method_mismatches"),
        ("order", "order_mismatches"),
    ):
        rows = report[key]
        if rows:
            lines.append(f"{label} (first 5):")
            for row in rows[:5]:
                lines.append(f"  - {json.dumps(row, ensure_ascii=False, sort_keys=True)}")
    return "\n".join(lines)


def _duplicate_keys(rows: list[dict]) -> set[tuple[str, str, int]]:
    return {(row["method"], row["path"], row["count"]) for row in rows}


def is_green(report: dict) -> bool:
    # Django itself owns every duplicate (two routers include APIRootView), so
    # Rust must reproduce that set exactly - not be duplicate-free.
    return not any(
        (
            report["missing"],
            report["extra"],
            report["param_mismatches"],
            report["method_mismatches"],
            report["order_mismatches"],
            _duplicate_keys(report["duplicate_rust"])
            != _duplicate_keys(report["duplicate_django"]),
            report["trailing_slash_collisions"]["rust"],
        )
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--django", type=Path, default=Path("contracts/routes.json"))
    parser.add_argument("--rust", type=Path, required=True)
    parser.add_argument("--check-order", action="store_true")
    parser.add_argument("--report-only", action="store_true")
    parser.add_argument("--json-out", type=Path)
    parser.add_argument("--allow-missing", action="store_true",
                        help="green while the Rust surface is still being ported")
    args = parser.parse_args(argv)

    report = compare(
        _django_routes(_load(args.django)),
        _rust_routes(_load(args.rust)),
        check_order=args.check_order,
    )
    print(summarize(report))

    if args.json_out:
        args.json_out.parent.mkdir(parents=True, exist_ok=True)
        args.json_out.write_text(
            json.dumps(report, indent=2, ensure_ascii=False, sort_keys=True) + "\n",
            encoding="utf-8",
            newline="",
        )

    if args.report_only:
        return 0
    if args.allow_missing:
        # Porting in progress: undeclared routes are expected. Everything else
        # (converter drift, method drift, trailing slash) must already be clean.
        trimmed = dict(
            report,
            missing=[],
            extra=[],
            duplicate_rust=report["duplicate_django"],
        )
        return 0 if is_green(trimmed) else 1
    return 0 if is_green(report) else 1


if __name__ == "__main__":
    sys.exit(main())
