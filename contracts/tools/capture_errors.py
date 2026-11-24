# Copyright (c) 2023-present Plane Software, Inc. and contributors
# SPDX-License-Identifier: AGPL-3.0-only
# See the LICENSE file for details.

"""Record Django/DRF error responses without Docker.

The capture uses the real Django 5.2.17 + DRF 3.18.3 packages and the repo\'s own
error machinery:

* `plane/app/views/error_404.py` (Django `handler404`, `JsonResponse`)
* `plane/middleware/request_body_size.py` (`JsonResponse`, 413)
* `plane/authentication/adapter/exception.py` (`auth_exception_handler`, 429)

Only the views are local: the routes are thin `APIView`s that raise or validate
the way Plane\'s endpoints do, so every status is produced by framework code, not
by a hand-written literal. The settings mirror the values in
`apps/api/plane/settings/common.py` that decide the wire bytes.

Run from the repo root:

    py contracts/tools/capture_errors.py

Output lands in `contracts/golden/error-*` with the same layout as the HTTP
captures (`request.http`, `response.http`, `body.json`, `body.raw`).

Dependencies: Django==5.2.17, djangorestframework==3.18.3 (see
`apps/api/pyproject.toml`). No database, Redis, or container is needed.
"""

import importlib.util
import json
import pathlib
import sys
import types

REPO = pathlib.Path(__file__).resolve().parents[2]
API_DIR = REPO / "apps" / "api"
OUT_DIR = REPO / "contracts" / "golden"

# `plane/__init__.py` imports Celery. Stub the package with its real __path__ so
# submodules resolve to repo code while the heavy __init__ is skipped.
plane_pkg = types.ModuleType("plane")
plane_pkg.__path__ = [str(API_DIR / "plane")]
sys.modules["plane"] = plane_pkg
sys.path.insert(0, str(API_DIR))

import django  # noqa: E402
from django.conf import settings  # noqa: E402

DATA_UPLOAD_MAX_MEMORY_SIZE = 5242880

settings.configure(
    DEBUG=False,
    ALLOWED_HOSTS=["*"],
    SECRET_KEY="contract-capture",
    ROOT_URLCONF=__name__,
    INSTALLED_APPS=[
        "django.contrib.contenttypes",
        "django.contrib.auth",
        "rest_framework",
    ],
    DATABASES={"default": {"ENGINE": "django.db.backends.sqlite3", "NAME": ":memory:"}},
    MIDDLEWARE=[
        "django.middleware.common.CommonMiddleware",
        "plane.middleware.request_body_size.RequestBodySizeLimitMiddleware",
    ],
    DATA_UPLOAD_MAX_MEMORY_SIZE=DATA_UPLOAD_MAX_MEMORY_SIZE,
    REST_FRAMEWORK={
        "DEFAULT_AUTHENTICATION_CLASSES": (
            "rest_framework.authentication.SessionAuthentication",
        ),
        "DEFAULT_PERMISSION_CLASSES": ("rest_framework.permissions.IsAuthenticated",),
        "DEFAULT_RENDERER_CLASSES": ("rest_framework.renderers.JSONRenderer",),
        "EXCEPTION_HANDLER": (
            "plane.authentication.adapter.exception.auth_exception_handler"
        ),
    },
)
django.setup()

from django.http import Http404  # noqa: E402
from django.test import Client  # noqa: E402
from django.urls import path  # noqa: E402
from rest_framework import serializers  # noqa: E402
from rest_framework.exceptions import PermissionDenied  # noqa: E402
from rest_framework.permissions import AllowAny  # noqa: E402
from rest_framework.response import Response  # noqa: E402
from rest_framework.throttling import BaseThrottle  # noqa: E402
from rest_framework.views import APIView  # noqa: E402


def load_handler(module_name, relative_path, attribute):
    """Import one function without importing the app package around it."""
    spec = importlib.util.spec_from_file_location(module_name, API_DIR / relative_path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return getattr(module, attribute)


handler404 = load_handler("plane_error_404", "plane/app/views/error_404.py", "custom_404_view")


class MeView(APIView):
    """Mirrors `UserEndpoint`: authenticated, and the same verb set, so the
    recorded `Allow` header matches the real `/api/users/me/`."""

    def get(self, request):
        return Response({"ok": True})

    def patch(self, request):
        return Response({"ok": True})

    def delete(self, request):
        return Response({"ok": True})


class ForbiddenView(APIView):
    permission_classes = [AllowAny]

    def get(self, request):
        raise PermissionDenied()


class MissingView(APIView):
    permission_classes = [AllowAny]

    def get(self, request):
        raise Http404()


class PublicView(APIView):
    permission_classes = [AllowAny]

    def get(self, request):
        return Response({"ok": True})


class Payload(serializers.Serializer):
    name = serializers.CharField()


class BodyView(APIView):
    permission_classes = [AllowAny]

    def post(self, request):
        serializer = Payload(data=request.data)
        serializer.is_valid(raise_exception=True)
        return Response({"ok": True})


class Broken(serializers.Serializer):
    def validate(self, attrs):
        raise serializers.ValidationError("Something went wrong.")


class NonFieldView(APIView):
    permission_classes = [AllowAny]

    def post(self, request):
        serializer = Broken(data=request.data)
        serializer.is_valid(raise_exception=True)
        return Response({"ok": True})


class RaisesValidationError(APIView):
    permission_classes = [AllowAny]

    def post(self, request):
        raise serializers.ValidationError("Something went wrong.")


class UnicodeView(APIView):
    permission_classes = [AllowAny]

    def get(self, request):
        return Response({"name": "Ren\u00e9 \u2014 \u4f60\u597d", "emoji": "\U0001f600"})


class AlwaysThrottled(BaseThrottle):
    def allow_request(self, request, view):
        return False

    def wait(self):
        return 12


class ThrottledView(APIView):
    permission_classes = [AllowAny]
    throttle_classes = [AlwaysThrottled]

    def get(self, request):
        return Response({"ok": True})


urlpatterns = [
    path("api/users/me/", MeView.as_view()),
    path("api/forbidden/", ForbiddenView.as_view()),
    path("api/missing/", MissingView.as_view()),
    path("api/public/", PublicView.as_view()),
    path("api/body/", BodyView.as_view()),
    path("api/non-field/", NonFieldView.as_view()),
    path("api/raised/", RaisesValidationError.as_view()),
    path("api/throttled/", ThrottledView.as_view()),
    path("api/unicode/", UnicodeView.as_view()),
]

UA = "Mozilla/5.0 (compatible; planesync/1.0)"
KEPT_HEADERS = ("content-type", "allow", "location")


def record(name, method, path_, response):
    """Write one case in the same shape as the HTTP captures."""
    body = response.content
    directory = OUT_DIR / name
    directory.mkdir(parents=True, exist_ok=True)

    request_lines = [
        f"{method} {path_} HTTP/1.1",
        "Host: 127.0.0.1:8000",
        f"User-Agent: {UA}",
        "Accept: application/json",
        "Cookie: <session cookie from live sign-in>",
        "",
    ]
    (directory / "request.http").write_text(
        "\n".join(request_lines) + "\n", encoding="utf-8", newline=""
    )

    headers = sorted(
        (key.title(), value)
        for key, value in response.items()
        if key.lower() in KEPT_HEADERS
    )
    response_lines = [f"HTTP/1.1 {response.status_code}"]
    response_lines += [f"{key}: {value}" for key, value in headers]
    (directory / "response.http").write_text(
        "\n".join(response_lines) + "\n\n", encoding="utf-8", newline=""
    )

    for stale in ("body.json", "body.raw"):
        target = directory / stale
        if target.exists():
            target.unlink()

    # `body.raw` keeps the exact wire bytes, spacing included. `body.json` is the
    # same body pretty-printed for review, so a human diff shows the keys without
    # hiding a separator change.
    (directory / "body.raw").write_bytes(body)
    try:
        parsed = json.loads(body)
    except ValueError:
        kind = "raw"
    else:
        (directory / "body.json").write_text(
            json.dumps(parsed, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
            newline="",
        )
        kind = "json"

    allow = response.get("Allow")
    print(f"{name}: HTTP {response.status_code} ct={response.get('Content-Type')} allow={allow} ({kind})")


def main():
    client = Client()

    record("error-401", "GET", "/api/users/me/", client.get("/api/users/me/"))
    record("error-403", "GET", "/api/forbidden/", client.get("/api/forbidden/"))
    record("error-404-drf", "GET", "/api/missing/", client.get("/api/missing/"))
    record(
        "error-404",
        "GET",
        "/api/does-not-exist/",
        client.get("/api/does-not-exist/"),
    )
    # 405 needs an authenticated caller; anonymously DRF answers 401 first.
    record("error-405", "DELETE", "/api/public/", client.delete("/api/public/"))
    record(
        "error-400-field",
        "POST",
        "/api/body/",
        client.post("/api/body/", data="{}", content_type="application/json"),
    )
    record(
        "error-400-non-field",
        "POST",
        "/api/non-field/",
        client.post("/api/non-field/", data="{}", content_type="application/json"),
    )
    record(
        "error-400-raised",
        "POST",
        "/api/raised/",
        client.post("/api/raised/", data="{}", content_type="application/json"),
    )
    record(
        "error-415",
        "POST",
        "/api/body/",
        client.post("/api/body/", data="x", content_type="text/plain"),
    )
    record("error-429", "GET", "/api/throttled/", client.get("/api/throttled/"))
    # Escaping is part of the contract: DRF keeps raw UTF-8, Django\'s JsonResponse
    # writes `\\uXXXX`. Every Django-owned body today (health/404/413) is ASCII, so
    # only the DRF case has a wire golden; the Django half is pinned by unit tests
    # in `plane_pkg::json` against the bytes measured here.
    record("unicode-drf", "GET", "/api/unicode/", client.get("/api/unicode/"))

    oversized = b'{"name":"' + b"x" * (DATA_UPLOAD_MAX_MEMORY_SIZE + 1024) + b'"}'
    record(
        "error-413",
        "POST",
        "/api/body/",
        client.post("/api/body/", data=oversized, content_type="application/json"),
    )


if __name__ == "__main__":
    main()
