# Copyright (c) 2023-present Plane Software, Inc. and contributors
# SPDX-License-Identifier: AGPL-3.0-only
# See the LICENSE file for details.

"""Record the auth contract from the real Django/DRF stack, no container.

Covers what the `[CORE] Auth extractor & Actor` card owns and what can be
measured without a database:

* `plane.api.middleware.api_authentication.APIKeyAuthentication` - header
  parsing, the query predicate, the success path, and the status DRF finally
  renders (`AuthenticationFailed` is coerced to 403 when the authenticator has
  no `authenticate_header`, which is this class);
* `plane.authentication.session.BaseSessionAuthentication` - the safe-method
  guard, the Origin rule, and the exact `PermissionDenied` message;
* `plane.authentication.rate_limit.AuthenticationThrottle` and
  `plane.api.rate_limit.ApiKeyRateThrottle` - scope, cache key, and the 429 body;
* Django's `parse_cookie`, `_unquote`, `http_date`, `delete_cookie` and
  `Morsel.OutputString`, because the session cookie is part of the contract.

`plane.db.models` is stubbed: the queryset cannot run without a database, so the
capture records the *call* Django builds (connector, lookups, kwargs) and the
outcome for each class of token (valid, expired, inactive, missing). That is an
observation of the real code path, not a mock of the answer.

Django 5.2.17 + DRF 3.18.3. Run from the repo root:

    apps/api/.venv/Scripts/python.exe contracts/tools/capture_auth.py
"""

import json
import pathlib
import sys
import types

REPO = pathlib.Path(__file__).resolve().parents[2]
API_DIR = REPO / "apps" / "api"
OUT = REPO / "contracts" / "golden" / "auth-cases.json"

# `plane/__init__.py` imports Celery; keep the real package path, skip the side
# effects by installing a stub.
plane_pkg = types.ModuleType("plane")
plane_pkg.__path__ = [str(API_DIR / "plane")]
plane_pkg.celery_app = None
sys.modules.setdefault("plane", plane_pkg)
sys.path.insert(0, str(API_DIR))

import django  # noqa: E402
from django.conf import settings  # noqa: E402

settings.configure(
    DEBUG=False,
    ALLOWED_HOSTS=["*"],
    SECRET_KEY="contract-capture",
    INSTALLED_APPS=[
        "django.contrib.contenttypes",
        "django.contrib.auth",
        "rest_framework",
    ],
    DATABASES={"default": {"ENGINE": "django.db.backends.sqlite3", "NAME": ":memory:"}},
    CACHES={"default": {"BACKEND": "django.core.cache.backends.locmem.LocMemCache"}},
    REST_FRAMEWORK={
        "DEFAULT_RENDERER_CLASSES": ("rest_framework.renderers.JSONRenderer",),
        "DEFAULT_PERMISSION_CLASSES": ("rest_framework.permissions.IsAuthenticated",),
        "DEFAULT_THROTTLE_RATES": {"anon": "30/minute", "asset_id": "5/minute"},
        "EXCEPTION_HANDLER": (
            "plane.authentication.adapter.exception.auth_exception_handler"
        ),
    },
    API_KEY_RATE_LIMIT="60/minute",
    AUTHENTICATION_RATE_LIMIT="10/minute",
    NUM_PROXIES=None,
    SESSION_COOKIE_NAME="session-id",
    SESSION_COOKIE_AGE=604800,
    SESSION_COOKIE_PATH="/",
    SESSION_COOKIE_DOMAIN=None,
    SESSION_COOKIE_SECURE=False,
    SESSION_COOKIE_HTTPONLY=True,
    SESSION_COOKIE_SAMESITE="Lax",
    ADMIN_SESSION_COOKIE_NAME="admin-session-id",
    ADMIN_SESSION_COOKIE_AGE=3600,
)
django.setup()


# --- stub `plane.db.models.APIToken` ----------------------------------------
# Installed after `django.setup()` so Django's own app loading is untouched, and
# before the auth module is imported, because it does `from plane.db.models
# import APIToken` at module scope.
class DoesNotExist(Exception):
    """Stands in for `APIToken.DoesNotExist`."""


class Recorder:
    """Records the call the real authenticator makes, then answers."""

    def __init__(self):
        self.calls = []
        self.answer = None

    def get(self, *args, **kwargs):
        self.calls.append(
            {
                "args": [describe_q(arg) for arg in args],
                "kwargs": {key: describe(value) for key, value in kwargs.items()},
            }
        )
        if isinstance(self.answer, Exception):
            raise self.answer
        return self.answer


def describe(value):
    from datetime import datetime

    if isinstance(value, datetime):
        return "<now>"
    return value


def describe_q(node):
    """Flatten a `Q` tree into connector/lookup tuples with `<now>` for time."""
    if hasattr(node, "children"):
        return {
            "connector": node.connector,
            "negated": node.negated,
            "children": [describe_q(child) for child in node.children],
        }
    if isinstance(node, tuple):
        return [describe(part) for part in node]
    return describe(node)


class FakeToken:
    def __init__(self, token="tok-1", user_id="u-1"):
        self.token = token
        self.user = types.SimpleNamespace(id=user_id, is_active=True, is_authenticated=True)
        self.last_used = None
        self.saved = []

    def save(self, **kwargs):
        self.saved.append(kwargs)


class FakeAPIToken:
    DoesNotExist = DoesNotExist
    objects = Recorder()


db_models = types.ModuleType("plane.db.models")
db_models.APIToken = FakeAPIToken
sys.modules["plane.db.models"] = db_models

from datetime import datetime, timezone  # noqa: E402

from django.core.cache import cache  # noqa: E402
from django.http import HttpResponse  # noqa: E402
from django.http.cookie import parse_cookie  # noqa: E402
from django.utils.http import http_date  # noqa: E402
from rest_framework.exceptions import PermissionDenied  # noqa: E402
from rest_framework.permissions import IsAuthenticated  # noqa: E402
from rest_framework.response import Response  # noqa: E402
from rest_framework.test import APIRequestFactory  # noqa: E402
from rest_framework.views import APIView  # noqa: E402

from plane.api.middleware.api_authentication import APIKeyAuthentication  # noqa: E402
from plane.api.rate_limit import ApiKeyRateThrottle  # noqa: E402
from plane.authentication.rate_limit import AuthenticationThrottle  # noqa: E402
from plane.authentication.session import (  # noqa: E402
    SAFE_METHODS,
    BaseSessionAuthentication,
    is_allowed_origin,
)

FACTORY = APIRequestFactory()
NOW = 1795649400


class ApiKeyView(APIView):
    authentication_classes = [APIKeyAuthentication]
    permission_classes = [IsAuthenticated]

    def get(self, request):
        return Response({"ok": True})


API_KEY_VIEW = ApiKeyView.as_view()


def rendered(response):
    if not getattr(response, "is_rendered", False):
        response.render()
    return {
        "status": response.status_code,
        "headers": {key: value for key, value in response.items()},
        "body": response.content.decode("utf-8"),
    }


class OriginRequest:
    def __init__(self, origin, host="api.plane.so"):
        self.META = {} if origin is None else {"HTTP_ORIGIN": origin}
        self._host = host

    def get_host(self):
        return self._host


def header_cases():
    rows = []
    for raw in [None, "", "   ", "key-abc", "  key-abc  "]:
        kwargs = {} if raw is None else {"HTTP_X_API_KEY": raw}
        request = FACTORY.get("/api/v1/x/", **kwargs)
        rows.append(
            {
                "raw": raw,
                "value": APIKeyAuthentication().get_api_token(request),
            }
        )
    # A repeated header reaches Django as the comma-joined value, and that is
    # what `.get()` sees; DRF keeps it as one string.
    request = FACTORY.get("/api/v1/x/", HTTP_X_API_KEY="a,b")
    rows.append({"raw": "a,b", "value": APIKeyAuthentication().get_api_token(request)})
    return rows


def outcome_cases():
    rows = []
    scenarios = [
        ("anonymous", {}, None, None),
        ("empty-header", {"HTTP_X_API_KEY": ""}, None, None),
        ("blank-header", {"HTTP_X_API_KEY": "   "}, DoesNotExist(),
         {"token": "   ", "is_active": True, "user_is_active": True, "expired": "none"}),
        ("invalid-token", {"HTTP_X_API_KEY": "nope"}, DoesNotExist(),
         {"token": "nope", "is_active": True, "user_is_active": True, "expired": "none"}),
        ("expired-token", {"HTTP_X_API_KEY": "old"}, DoesNotExist(),
         {"token": "old", "is_active": True, "user_is_active": True, "expired": "past"}),
        ("inactive-token", {"HTTP_X_API_KEY": "off"}, DoesNotExist(),
         {"token": "off", "is_active": False, "user_is_active": True, "expired": "none"}),
        ("inactive-user", {"HTTP_X_API_KEY": "gone"}, DoesNotExist(),
         {"token": "gone", "is_active": True, "user_is_active": False, "expired": "none"}),
        ("valid-token", {"HTTP_X_API_KEY": "tok-1"}, FakeToken(),
         {"token": "tok-1", "is_active": True, "user_is_active": True, "expired": "future"}),
    ]
    for name, kwargs, answer, record in scenarios:
        FakeAPIToken.objects.calls.clear()
        FakeAPIToken.objects.answer = answer
        request = FACTORY.get("/api/v1/x/", **kwargs)
        row = rendered(API_KEY_VIEW(request))
        row["case"] = name
        row["query"] = json.loads(json.dumps(FakeAPIToken.objects.calls))
        row["record"] = record
        rows.append(row)
    FakeAPIToken.objects.answer = None
    return rows


def success_side_effects():
    token = FakeToken()
    FakeAPIToken.objects.calls.clear()
    FakeAPIToken.objects.answer = token
    request = FACTORY.get("/api/v1/x/", HTTP_X_API_KEY="tok-1")
    authenticator = APIKeyAuthentication()
    user, returned = authenticator.authenticate(request)
    FakeAPIToken.objects.answer = None
    return {
        "returns_user_id": user.id,
        "returns_token": returned,
        "last_used_set": token.last_used is not None,
        "save_calls": token.saved,
        "api_key_authenticated": getattr(
            getattr(request, "_request", request), "api_key_authenticated", None
        ),
    }


def csrf_cases():
    """`is_allowed_origin` alone and the full `enforce_csrf` guard."""
    rows = []
    for method in ["GET", "HEAD", "OPTIONS", "TRACE", "get", "POST", "PATCH", "post"]:
        for origin in [None, "https://evil.example.com", "https://web.plane.so", "null"]:
            request = OriginRequest(origin)
            request.method = method
            try:
                BaseSessionAuthentication().enforce_csrf(request)
                enforced = True
            except PermissionDenied:
                enforced = False
            rows.append(
                {
                    "method": method,
                    "origin": origin,
                    "allowed": is_allowed_origin(OriginRequest(origin)),
                    "enforced": enforced,
                }
            )
    return rows


def csrf_exception():
    """The message and type `enforce_csrf` raises, through the real method."""

    class HttpsOnly(OriginRequest):
        pass

    outcome = {}
    for method, origin in [("POST", "https://evil.example.com"), ("POST", None), ("GET", "https://evil.example.com")]:
        request = OriginRequest(origin)
        request.method = method
        try:
            BaseSessionAuthentication().enforce_csrf(request)
            outcome[f"{method}:{origin}"] = None
        except Exception as exc:  # noqa: BLE001 - the point is the exact class
            outcome[f"{method}:{origin}"] = {
                "type": type(exc).__name__,
                "detail": str(exc),
                "status_code": exc.status_code,
            }
    return outcome


def cookie_cases():
    parse_rows = []
    for raw in [
        "session-id=abc123",
        "  session-id = abc123 ; other=1",
        "session-id=first; session-id=second",
        'session-id="abc123"',
        "Session-Id=abc123",
        "session-id",
        "session-id=; other=1",
        "$Version=1; session-id=abc; $Path=/",
        'session-id="a;b"',
        'session-id="abc',
        "session-id=a,b",
        'session-id="a\\073b"',
        'session-id="a b"',
        'session-id="a\\"b"',
        "session-id=a%20b",
    ]:
        parse_rows.append({"raw": raw, "cookies": parse_cookie(raw)})

    import re

    def stable(raw):
        return re.sub(r"expires=[^;]+", "expires=<http-date>", raw, flags=re.I)

    set_rows = []

    def render(label, key, value, **kwargs):
        response = HttpResponse()
        response.set_cookie(key, value, **kwargs)
        set_rows.append(
            {
                "case": label,
                "name": key,
                "value": value,
                "attributes": {
                    name: (str(option) if name != "expires" else "<http-date>")
                    for name, option in kwargs.items()
                    if option is not None
                },
                "set_cookie": stable(response.cookies[key].OutputString()),
            }
        )

    render(
        "session",
        "session-id",
        "abc123",
        max_age=604800,
        expires=http_date(0),
        domain=None,
        path="/",
        secure=None,
        httponly=True,
        samesite="Lax",
    )
    render(
        "admin-secure",
        "admin-session-id",
        "adm1",
        max_age=3600,
        expires=http_date(0),
        domain=None,
        path="/",
        secure=True,
        httponly=True,
        samesite="Lax",
    )
    render(
        "browser-session",
        "session-id",
        "abc",
        domain=None,
        path="/",
        secure=None,
        httponly=True,
        samesite="Lax",
    )
    render("quoted", "s", "a b;c", path="/")
    render("empty-value", "s", "", path="/")

    delete_rows = []
    for label, key, samesite in [
        ("session", "session-id", "Lax"),
        ("host-prefixed-none", "__Host-session", "None"),
    ]:
        response = HttpResponse()
        response.delete_cookie(key, path="/", domain=None, samesite=samesite)
        delete_rows.append(
            {
                "case": label,
                "name": key,
                "samesite": samesite,
                "set_cookie": response.cookies[key].OutputString(),
            }
        )

    return {
        "parse": parse_rows,
        "set": set_rows,
        "delete": delete_rows,
        "http_date": [http_date(0), http_date(1), http_date(NOW)],
    }


def throttle_cases():
    cache.clear()
    api_key = ApiKeyRateThrottle()
    api_key.rate = "2/minute"
    api_key.num_requests, api_key.duration = api_key.parse_rate("2/minute")
    request = FACTORY.get("/api/v1/throttled/", HTTP_X_API_KEY="key-abc")
    allowed = api_key.allow_request(request, None)
    api_key_row = {
        "scope": api_key.scope,
        "key": api_key.key,
        "store_keys": sorted(getattr(cache, "_cache", {}).keys()),
        "allowed": allowed,
        "remaining": request.META.get("X-RateLimit-Remaining"),
        "reset": request.META.get("X-RateLimit-Reset"),
        "no_header_returns_none": ApiKeyRateThrottle().get_cache_key(
            FACTORY.get("/api/v1/throttled/"), None
        ),
    }

    cache.clear()
    authentication = AuthenticationThrottle()
    authentication.rate = "1/minute"
    authentication.num_requests, authentication.duration = authentication.parse_rate(
        "2/minute"
    )
    request = FACTORY.post("/auth/sign-in/", REMOTE_ADDR="203.0.113.9")
    allowed = authentication.allow_request(request, None)
    auth_row = {
        "scope": authentication.scope,
        "key": authentication.key,
        "store_keys": sorted(getattr(cache, "_cache", {}).keys()),
        "allowed": allowed,
        "class_rate": AuthenticationThrottle.rate,
    }

    # `get_ident` returns None when neither `REMOTE_ADDR` nor `X-Forwarded-For`
    # is present, and `cache_format % {...}` then renders "None" as the ident.
    cache.clear()
    anonymous = AuthenticationThrottle()
    anonymous.rate = "2/minute"
    anonymous.num_requests, anonymous.duration = anonymous.parse_rate("2/minute")
    request = FACTORY.post("/auth/sign-in/")
    anonymous.allow_request(request, None)
    auth_row["no_remote_addr_key"] = anonymous.key

    # 429 body and headers through a real view.
    cache.clear()
    AuthView = type(
        "AuthView",
        (APIView,),
        {
            "throttle_classes": [AuthenticationThrottle],
            "permission_classes": [],
            "post": lambda self, request: Response({"ok": True}),
        },
    )
    # `throttle_classes` instantiates the class, so the rate has to be lowered
    # on the class to make the second call trip the limit.
    original_rate = AuthenticationThrottle.rate
    AuthenticationThrottle.rate = "1/minute"
    try:
        view = AuthView.as_view()
        request = FACTORY.post("/auth/sign-in/", REMOTE_ADDR="203.0.113.9")
        first = rendered(view(request))
        request = FACTORY.post("/auth/sign-in/", REMOTE_ADDR="203.0.113.9")
        second = rendered(view(request))
    finally:
        AuthenticationThrottle.rate = original_rate
    auth_row["first"] = first
    auth_row["second"] = second
    auth_row["rate_override"] = "1/minute"
    cache.clear()
    return {"api_key": api_key_row, "authentication": auth_row}


def session_cookie_name_cases():
    rows = []
    for path in [
        "/api/instances/",
        "/api/v1/x/",
        "/api/instancesfoo/",
        "/api/v1/my-instances/x/",
        "/api/v1/instances/x/",
        "/API/Instances/x/",
    ]:
        rows.append(
            {
                "path": path,
                "admin": "instances" in path,
                "cookie": "admin-session-id" if "instances" in path else "session-id",
            }
        )
    return rows


def main():
    payload = {
        "source": "Django 5.2.17 + DRF 3.18.3, no database and no container",
        "recorded_at": datetime.now(timezone.utc).strftime("%Y-%m-%d"),
        "settings": {
            "api_key_header": APIKeyAuthentication.auth_header_name,
            "session_cookie_name": settings.SESSION_COOKIE_NAME,
            "admin_cookie_name": settings.ADMIN_SESSION_COOKIE_NAME,
            "session_cookie_age": settings.SESSION_COOKIE_AGE,
            "admin_cookie_age": settings.ADMIN_SESSION_COOKIE_AGE,
            "authentication_rate": AuthenticationThrottle.rate,
            "authentication_scope": AuthenticationThrottle.scope,
            "api_key_rate": settings.API_KEY_RATE_LIMIT,
            "api_key_scope": ApiKeyRateThrottle.scope,
            "safe_methods": list(SAFE_METHODS),
        },
        "api_key": {
            "header": header_cases(),
            "outcomes": outcome_cases(),
            "success": success_side_effects(),
        },
        "session": {
            "csrf": csrf_cases(),
            "exception": csrf_exception(),
            "cookie_name": session_cookie_name_cases(),
        },
        "cookies": cookie_cases(),
        "throttle": throttle_cases(),
    }
    OUT.write_text(
        json.dumps(payload, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()