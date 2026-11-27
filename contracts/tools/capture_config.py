# Copyright (c) 2023-present Plane Software, Inc. and contributors
# SPDX-License-Identifier: AGPL-3.0-only
# See the LICENSE file for details.

"""Record the env-parsing helpers from Django's settings, without Django.

The oracle is the *text* of ``apps/api/plane/settings/common.py``: the helper
functions are pulled out with ``ast`` and executed in an empty namespace, so the
answers come from Plane's own code rather than from a re-typed copy. Importing
Django is not required, and nothing here touches a database or a container.

Run from the repo root:

    apps\\api\\.venv\\Scripts\\python.exe contracts\\tools\\capture_config.py

The output is ``contracts/golden/config-helpers.json``; re-running it must not
change a byte. ``crates/pkg/src/config.rs`` asserts the same rows in
``config::tests``.

Use the interpreter the API targets (``apps/api/pyproject.toml`` says
``>=3.12,<3.13``). ``urlparse`` treats a digit-leading scheme differently on
3.10 and 3.12, so another interpreter would silently move the golden; the
recorded ``python`` field makes that visible.
"""

import ast
import json
import pathlib
import sys
from urllib.parse import parse_qs, urlparse

REPO = pathlib.Path(__file__).resolve().parents[2]
COMMON = REPO / "apps" / "api" / "plane" / "settings" / "common.py"
URL_MODULE = REPO / "apps" / "api" / "plane" / "utils" / "url.py"
OUT = REPO / "contracts" / "golden" / "config-helpers.json"

HELPERS = ("_cors_settings", "_cookies_secure", "_redis_ssl_kwargs", "_retention_days")


class _EnvProxy:
    """Stands in for ``os`` so the helpers read a mapping we control."""

    def __init__(self):
        self.environ = {}


def load_helpers():
    """Execute the named functions from common.py / url.py in a bare namespace."""
    tree = ast.parse(COMMON.read_text(encoding="utf-8"))
    body = [
        node
        for node in tree.body
        if isinstance(node, ast.FunctionDef) and node.name in HELPERS
    ]
    url_tree = ast.parse(URL_MODULE.read_text(encoding="utf-8"))
    body += [
        node
        for node in url_tree.body
        if isinstance(node, ast.FunctionDef) and node.name == "is_valid_url"
    ]
    missing = set(HELPERS) - {node.name for node in body}
    if missing:
        raise SystemExit(f"helper moved or renamed: {sorted(missing)}")

    namespace = {
        "os": _EnvProxy(),
        "urlparse": urlparse,
        "parse_qs": parse_qs,
        "ValueError": ValueError,
        "TypeError": TypeError,
        "bool": bool,
        "int": int,
        "any": any,
        "str": str,
        "len": len,
        "frozenset": frozenset,
    }
    exec(
        compile(ast.Module(body=body, type_ignores=[]), str(COMMON), "exec"), namespace
    )
    return namespace


def split_origins(raw):
    return [origin.strip() for origin in raw.split(",") if origin.strip()]


def main():
    ns = load_helpers()
    cors = ns["_cors_settings"]
    secure = ns["_cookies_secure"]
    redis_ssl = ns["_redis_ssl_kwargs"]
    valid_url = ns["is_valid_url"]

    url_cases = [
        "https://example.com",
        "http://localhost:3000/path",
        "https://user:pass@example.com/x",
        "https://:8080",
        "custom+v1.2-x://host",
        "mailto:x@y.test",
        "example.com",
        "https://",
        "https:///path",
        "https:x",
        "",
        "/relative",
        "1http://host",
        "ht tp://host",
    ]

    cors_cases = []
    for raw in [
        "https://a.test, https://b.test ,https://a.test",
        "",
        " , ",
    ]:
        configured = split_origins(raw)
        for base_urls in [
            [],
            ["https://app.example.com"],
            ["https://app.example.com", "http://localhost:3000", None, "", "not a url"],
            ["HTTPS://App.Example.COM/Path?q=1"],
        ]:
            origins, allow_all, allow_credentials = cors(configured, base_urls)
            cors_cases.append(
                {
                    "raw": raw,
                    "base_urls": ["" if url is None else url for url in base_urls],
                    "origins": origins,
                    "allow_all": allow_all,
                    "allow_credentials": allow_credentials,
                }
            )

    secure_cases = []
    for web_url, app_base_url, raw in [
        ("https://app.example.com", None, ""),
        ("http://localhost:3000", None, "https://a.test"),
        ("http://localhost:3000", None, "http://localhost:8080"),
        ("  ", "https://app.example.com", "https://app.example.com"),
        ("", "https://app.example.com", ""),
        (None, None, ""),
        ("HTTP://host", None, ""),
    ]:
        secure_cases.append(
            {
                "web_url": web_url,
                "app_base_url": app_base_url,
                "origins": split_origins(raw),
                "secure": secure(
                    web_url, split_origins(raw), app_base_url=app_base_url
                ),
            }
        )

    redis_cases = []
    for url, env in [
        ("rediss://r:6379/0?ssl_cert_reqs=none", {"REDIS_SSL_CERT_REQS": "required"}),
        ("rediss://r:6379/0", {"REDIS_SSL_CERT_REQS": "NONE"}),
        ("rediss://r:6379/0?ssl_cert_reqs=", {"REDIS_SSL_CERT_REQS": "none"}),
        ("rediss://r:6379/0?ssl_cert_reqs=%6Eone", {}),
        (
            "rediss://r:6379/0?db=0&ssl_cert_reqs=required",
            {"REDIS_SSL_CERT_REQS": "none"},
        ),
        ("redis://r:6379/0", {}),
        ("redis://r:6379/0", {"REDIS_SSL_CA_CERTS": "/ca.pem"}),
        ("redis://r:6379/0", {"REDIS_SSL_CA_CERTS": ""}),
        (None, {"REDIS_SSL_CERT_REQS": "  REQUIRED  "}),
    ]:
        ns["os"].environ = dict(env)
        redis_cases.append({"url": url, "env": env, "kwargs": redis_ssl(url)})

    retention_cases = []
    for raw, default in [
        (None, 60),
        ("30", 60),
        ("-5", 60),
        ("nope", 60),
        (" 0 ", 60),
        ("14", 7),
    ]:
        ns["os"].environ = {} if raw is None else {"RETENTION_UNDER_TEST": raw}
        retention_cases.append(
            {
                "raw": raw,
                "default": default,
                "days": ns["_retention_days"]("RETENTION_UNDER_TEST", default),
            }
        )

    case = json.loads(
        json.dumps(
            {
                "schema": "plane/config-helpers@1",
                "python": ".".join(str(part) for part in sys.version_info[:3]),
                "source": "apps/api/plane/settings/common.py, apps/api/plane/utils/url.py",
                "is_valid_url": {url: valid_url(url) for url in url_cases},
                "cors": cors_cases,
                "cookies_secure": secure_cases,
                "redis_ssl": redis_cases,
                "retention_days": retention_cases,
            },
            indent=1,
        )
    )
    OUT.write_text(json.dumps(case, indent=1) + "\n", encoding="utf-8", newline="\n")
    print(f"wrote {OUT.relative_to(REPO)}")


if __name__ == "__main__":
    main()
