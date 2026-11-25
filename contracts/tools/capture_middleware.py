# Copyright (c) 2023-present Plane Software, Inc. and contributors
# SPDX-License-Identifier: AGPL-3.0-only
# See the LICENSE file for details.

"""Record the middleware contract from the real Django stack, no container.

Covers the four middlewares the `[CORE] Middleware` card owns:

* `corsheaders.middleware.CorsMiddleware` (preflight, simple request, denied origin)
* `django.middleware.security.SecurityMiddleware` + `XFrameOptionsMiddleware`
* `django.middleware.gzip.GZipMiddleware`
* `plane.middleware.request_body_size.RequestBodySizeLimitMiddleware` (413)
* DRF `ApiKeyRateThrottle` / `AnonRateThrottle` (429 + `X-RateLimit-*`)

Django 5.2.17, django-cors-headers 4.9.0, DRF 3.18.3. No database, Redis, or
Docker: the cache is Django's in-memory `LocMemCache`.

Run from the repo root:

    apps/api/.venv/Scripts/python.exe contracts/tools/capture_middleware.py

Output goes to `contracts/golden/middleware-cases.json`.
"""

import gzip
import json
import pathlib
import sys
import types

REPO = pathlib.Path(__file__).resolve().parents[2]
API_DIR = REPO / "apps" / "api"
OUT = REPO / "contracts" / "golden" / "middleware-cases.json"

# `plane/__init__.py` imports Celery; keep the real package path, skip the side
# effects by installing a stub.
plane_pkg = types.ModuleType("plane")
plane_pkg.__path__ = [str(API_DIR / "plane")]
plane_pkg.celery_app = None
sys.modules.setdefault("plane", plane_pkg)
sys.path.insert(0, str(API_DIR))

import django  # noqa: E402
from django.conf import settings  # noqa: E402

WEB = "https://web.plane.so"
ADMIN = "https://admin.plane.so"
SPACE = "https://space.plane.so"
EVIL = "https://evil.example.com"

settings.configure(
    DEBUG=False,
    ALLOWED_HOSTS=["*"],
    SECRET_KEY="contract-capture",
    ROOT_URLCONF=__name__,
    INSTALLED_APPS=[
        "django.contrib.contenttypes",
        "django.contrib.auth",
        "corsheaders",
        "rest_framework",
    ],
    DATABASES={"default": {"ENGINE": "django.db.backends.sqlite3", "NAME": ":memory:"}},
    CACHES={"default": {"BACKEND": "django.core.cache.backends.locmem.LocMemCache"}},
    MIDDLEWARE=[
        "corsheaders.middleware.CorsMiddleware",
        "django.middleware.security.SecurityMiddleware",
        "django.middleware.common.CommonMiddleware",
        "django.middleware.clickjacking.XFrameOptionsMiddleware",
        "django.middleware.gzip.GZipMiddleware",
        "plane.middleware.request_body_size.RequestBodySizeLimitMiddleware",
    ],
    # Same values as `plane/settings/common.py`.
    CORS_ALLOWED_ORIGINS=[WEB, ADMIN, SPACE],
    CORS_ALLOW_ALL_ORIGINS=False,
    CORS_ALLOW_CREDENTIALS=True,
    CORS_ALLOW_HEADERS=[
        "accept",
        "authorization",
        "content-type",
        "user-agent",
        "x-csrftoken",
        "x-requested-with",
        "X-API-Key",
    ],
    DATA_UPLOAD_MAX_MEMORY_SIZE=5242880,
    REST_FRAMEWORK={
        "DEFAULT_RENDERER_CLASSES": ("rest_framework.renderers.JSONRenderer",),
        "DEFAULT_AUTHENTICATION_CLASSES": (
            "rest_framework.authentication.SessionAuthentication",
        ),
        "DEFAULT_PERMISSION_CLASSES": ("rest_framework.permissions.AllowAny",),
        "DEFAULT_THROTTLE_CLASSES": ("rest_framework.throttling.AnonRateThrottle",),
        "DEFAULT_THROTTLE_RATES": {"anon": "30/minute", "asset_id": "5/minute"},
        "EXCEPTION_HANDLER": (
            "plane.authentication.adapter.exception.auth_exception_handler"
        ),
    },
    API_KEY_RATE_LIMIT="60/minute",
    NUM_PROXIES=None,
)
django.setup()

from django.http import HttpResponse, JsonResponse  # noqa: E402
from django.test import Client  # noqa: E402
from django.urls import path  # noqa: E402
from rest_framework.response import Response  # noqa: E402
from rest_framework.views import APIView  # noqa: E402

from plane.api.rate_limit import ApiKeyRateThrottle  # noqa: E402

UA = "Mozilla/5.0 (compatible; planesync/1.0)"


def health(request):
    return JsonResponse({"status": "OK"})


def big(request):
    return HttpResponse("x" * 400, content_type="text/plain")


def gzip_body(request):
    return HttpResponse("a" * 500, content_type="text/plain")


class Throttled(APIView):
    throttle_classes = [ApiKeyRateThrottle]

    def get(self, request):
        return Response({"ok": True})


def custom_404(request, exception):
    return JsonResponse({"error": "Page not found."}, status=404)


# A module-level module object so `handler404` can be a dotted path.
handler404 = "capture_middleware.custom_404"
sys.modules.setdefault("capture_middleware", sys.modules[__name__])

urlpatterns = [
    path("", health),
    path("big/", big),
    path("text/", gzip_body),
    path("api/throttled/", Throttled.as_view()),
]


def headers_of(response, keep):
    return {
        key: value
        for key, value in response.items()
        if key.lower() in keep
    }


KEEP = {
    "content-type",
    "content-encoding",
    "content-length",
    "vary",
    "access-control-allow-origin",
    "access-control-allow-credentials",
    "access-control-allow-headers",
    "access-control-allow-methods",
    "access-control-max-age",
    "x-frame-options",
    "x-content-type-options",
    "referrer-policy",
    "cross-origin-opener-policy",
    "x-ratelimit-remaining",
    "x-ratelimit-reset",
}


def case(name, method, path_, client, **extra):
    response = getattr(client, method)(path_, **extra)
    body = response.content
    record = {
        "name": name,
        "method": method.upper(),
        "path": path_,
        "status": response.status_code,
        "headers": headers_of(response, KEEP),
        "body_sha256": __import__("hashlib").sha256(body).hexdigest(),
        "body_len": len(body),
    }
    if response.status_code == 200 and response.get("Content-Encoding") == "gzip":
        record["body_gunzipped"] = gzip.decompress(body).decode("latin-1")
        # The gzip header carries a random FNAME byte string; the contract is
        # the payload after decompression plus `Content-Encoding`.
        record["gzip_header_flags"] = body[3]
        record["gzip_mtime"] = int.from_bytes(body[4:8], "little")
    else:
        record["body_text"] = body.decode("utf-8", "replace")
    return record


def main():
    from django.contrib.auth.models import AnonymousUser
    from django.test.client import RequestFactory

    client = Client()
    cases = []

    cases.append(case("preflight-web", "options", "/", client,
                      HTTP_ORIGIN=WEB,
                      HTTP_ACCESS_CONTROL_REQUEST_METHOD="POST",
                      HTTP_ACCESS_CONTROL_REQUEST_HEADERS="content-type, x-api-key"))
    cases.append(case("preflight-admin", "options", "/", client,
                      HTTP_ORIGIN=ADMIN,
                      HTTP_ACCESS_CONTROL_REQUEST_METHOD="PATCH"))
    cases.append(case("preflight-space", "options", "/", client,
                      HTTP_ORIGIN=SPACE,
                      HTTP_ACCESS_CONTROL_REQUEST_METHOD="GET"))
    cases.append(case("preflight-evil", "options", "/", client,
                      HTTP_ORIGIN=EVIL,
                      HTTP_ACCESS_CONTROL_REQUEST_METHOD="POST"))

    cases.append(case("simple-web", "get", "/", client, HTTP_ORIGIN=WEB))
    cases.append(case("simple-evil", "get", "/", client, HTTP_ORIGIN=EVIL))
    cases.append(case("simple-no-origin", "get", "/", client))
    cases.append(case("simple-null-origin", "get", "/", client, HTTP_ORIGIN="null"))
    cases.append(case("simple-upper-origin", "get", "/", client,
                      HTTP_ORIGIN="https://WEB.plane.so"))
    cases.append(case("simple-trailing-slash", "get", "/", client,
                      HTTP_ORIGIN=WEB + "/"))
    cases.append(case("simple-subpath-origin", "get", "/", client,
                      HTTP_ORIGIN=WEB + "/some/path"))
    cases.append(case("options-no-acrm", "options", "/", client, HTTP_ORIGIN=WEB))
    cases.append(case("head-web", "head", "/", client, HTTP_ORIGIN=WEB))
    cases.append(case("post-robots", "post", "/robots.txt", client, HTTP_ORIGIN=WEB))

    cases.append(case("gzip-accepted", "get", "/text/", client,
                      HTTP_ACCEPT_ENCODING="gzip, deflate"))
    cases.append(case("gzip-not-accepted", "get", "/text/", client,
                      HTTP_ACCEPT_ENCODING="br"))
    cases.append(case("gzip-short-body", "get", "/", client,
                      HTTP_ACCEPT_ENCODING="gzip"))
    cases.append(case("gzip-q-zero", "get", "/text/", client,
                      HTTP_ACCEPT_ENCODING="gzip;q=0"))
    cases.append(case("gzip-xgzip", "get", "/text/", client,
                      HTTP_ACCEPT_ENCODING="xgzip"))
    cases.append(case("gzip-uppercase", "get", "/text/", client,
                      HTTP_ACCEPT_ENCODING="GZIP"))

    cases.append({
        "name": "body-too-large",
        "method": "POST",
        "path": "/big/",
        "status": None,
        "note": "captured separately: the client refuses the body before send",
    })

    # 413 through the whole stack: CORS, security, gzip and the body-size
    # middleware all see the response, exactly like a real request.
    cases[-1] = case(
        "body-too-large",
        "post",
        "/big/",
        client,
        data=b"x" * (5242880 + 1),
        content_type="application/octet-stream",
        HTTP_ORIGIN=WEB,
    )

    cases.append(case("not-found", "get", "/missing/", client, HTTP_ORIGIN=WEB))
    cases.append(case("not-found-no-origin", "get", "/missing/", client))

    # Rate limit: the same API key twice with a rate of 2/minute, then a third
    # request that must be throttled. DRF stores a list in the cache.
    from django.core.cache import cache  # noqa: E402
    from rest_framework.throttling import AnonRateThrottle  # noqa: E402
    from django.test import override_settings  # noqa: E402

    with override_settings(
        REST_FRAMEWORK={
            **settings.REST_FRAMEWORK,
            "DEFAULT_THROTTLE_RATES": {"anon": "2/minute", "asset_id": "5/minute"},
        }
    ):
        cache.clear()
        throttle = ApiKeyRateThrottle()
        throttle.rate = "2/minute"
        throttle.num_requests, throttle.duration = throttle.parse_rate("2/minute")
        factory = RequestFactory()
        observations = []
        for _ in range(3):
            request = factory.get("/api/throttled/", HTTP_X_API_KEY="key-abc")
            allowed = throttle.allow_request(request, None)
            observations.append({
                "allowed": allowed,
                "remaining": request.META.get("X-RateLimit-Remaining"),
                "reset": request.META.get("X-RateLimit-Reset"),
            })
        cases.append({
            "name": "api-key-throttle",
            "method": "GET",
            "path": "/api/throttled/",
            "rate": "2/minute",
            "observations": observations,
            "cache_key": "throttle_api_key_key-abc",
        })

        cache.clear()
        anon = AnonRateThrottle()
        anon.rate = "2/minute"
        anon.num_requests, anon.duration = anon.parse_rate("2/minute")
        request = RequestFactory().get("/api/throttled/", REMOTE_ADDR="203.0.113.9")
        request.user = AnonymousUser()
        observations = []
        for _ in range(3):
            allowed = anon.allow_request(request, None)
            observations.append({"allowed": allowed})
        cases.append({
            "name": "anon-throttle",
            "method": "GET",
            "path": "/api/throttled/",
            "rate": "2/minute",
            "observations": observations,
            "cache_key": "throttle_anon_203.0.113.9",
        })

    # 429 through DRF: the auth exception handler rewrites body and status.
    # `ApiKeyRateThrottle.rate` is captured at import time from
    # `settings.API_KEY_RATE_LIMIT`, so the rate is lowered on the class.
    original_rate = ApiKeyRateThrottle.rate
    ApiKeyRateThrottle.rate = "1/minute"
    try:
        cache.clear()
        client429 = Client()
        first = client429.get("/api/throttled/", HTTP_X_API_KEY="key-429")
        second = client429.get("/api/throttled/", HTTP_X_API_KEY="key-429")
    finally:
        ApiKeyRateThrottle.rate = original_rate
    cases.append({
        "name": "api-key-429",
        "method": "GET",
        "path": "/api/throttled/",
        "rate": "1/minute",
        "first_status": first.status_code,
        "first_headers": dict(first.items()),
        "status": second.status_code,
        "headers": dict(second.items()),
        "body_text": second.content.decode("utf-8"),
    })

    # Origin check that replaces CSRF (`plane/authentication/session.py`).
    from django.test import override_settings as override_origin  # noqa: E402
    from plane.authentication.session import is_allowed_origin  # noqa: E402
    from plane.authentication.session import normalize_origin  # noqa: E402

    with override_origin(
        WEB_URL=WEB,
        APP_BASE_URL=None,
        ADMIN_BASE_URL=ADMIN,
        SPACE_BASE_URL=None,
        LIVE_BASE_URL=None,
        CORS_ALLOWED_ORIGINS=[WEB],
        CSRF_TRUSTED_ORIGINS=[WEB],
    ):
        from django.core.cache import cache as cache_mod  # noqa: E402

        # `allowed_origins` is `lru_cache`d on the settings above.
        from plane.authentication import session as session_mod  # noqa: E402

        session_mod.allowed_origins.cache_clear()

        class FakeRequest:
            def __init__(self, method, origin, host="api.plane.so"):
                self.method = method
                self.META = {} if origin is None else {"HTTP_ORIGIN": origin}
                self._host = host

            def get_host(self):
                return self._host

        origin_cases = []
        for method, origin, host in [
            ("GET", "https://evil.example.com", "api.plane.so"),
            ("POST", None, "api.plane.so"),
            ("POST", "http://api.plane.so", "api.plane.so"),
            ("POST", "https://API.plane.so", "api.plane.so"),
            ("POST", "http://api.plane.so:8080", "api.plane.so"),
            ("PATCH", WEB + "/", "api.plane.so"),
            ("POST", WEB + "/a/b", "api.plane.so"),
            ("POST", ADMIN, "api.plane.so"),
            ("POST", "https://evil.example.com", "api.plane.so"),
            ("POST", "null", "api.plane.so"),
            ("POST", "not a url", "api.plane.so"),
            ("POST", "http://[::1", "api.plane.so"),
        ]:
            origin_cases.append({
                "method": method,
                "origin": origin,
                "host": host,
                "allowed": is_allowed_origin(FakeRequest(method, origin, host)),
            })
        cases.append({"name": "origin-check", "observations": origin_cases})

        normalizations = []
        for raw in [
            "https://WEB.plane.so",
            "https://web.plane.so/",
            "https://web.plane.so:443",
            "https://web.plane.so/a/b?x=1#f",
            "web.plane.so",
            "//web.plane.so",
            "https:/web.plane.so",
            "localhost:3000",
            "null",
            "",
            "http://[::1",
            "https://:8080",
        ]:
            normalizations.append({"raw": raw, "normalized": normalize_origin(raw)})
        cases.append({"name": "origin-normalize", "observations": normalizations})

    payload = {
        "generated_by": "contracts/tools/capture_middleware.py",
        "django": django.get_version(),
        "settings": {
            "cors_allowed_origins": [WEB, ADMIN, SPACE],
            "cors_allow_all": False,
            "cors_allow_credentials": True,
            "cors_allow_headers": list(settings.CORS_ALLOW_HEADERS),
            "cors_allow_methods": list(
                getattr(settings, "CORS_ALLOW_METHODS", None)
                or ["DELETE", "GET", "OPTIONS", "PATCH", "POST", "PUT"]
            ),
            "cors_preflight_max_age": 86400,
            "data_upload_max_memory_size": 5242880,
            "file_size_limit": 5242880,
            "num_proxies": None,
        },
        "cases": cases,
    }
    OUT.write_text(
        json.dumps(payload, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    print(f"wrote {OUT} ({len(cases)} cases)")


if __name__ == "__main__":
    main()
