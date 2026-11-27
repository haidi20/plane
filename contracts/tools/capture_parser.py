# Copyright (c) 2023-present Plane Software, Inc. and contributors
# SPDX-License-Identifier: AGPL-3.0-only
# See the LICENSE file for details.

"""Record DRF's request-parser behaviour without Docker.

The oracle is the real ``rest_framework.request.Request`` with DRF's default
parser set (JSON, form, multipart). Every case goes through a real ``APIView``
so the 415/400 statuses and bodies come from DRF, not from this script.

Run from the repo root with the ``apps/api`` venv:

    apps\\api\\.venv\\Scripts\\python.exe contracts\\tools\\capture_parser.py

Cases are written under ``contracts/golden/parser-*``. The file is
deterministic: re-running it must not change a byte.
"""

import json
import pathlib
import sys

REPO = pathlib.Path(__file__).resolve().parents[2]
API_DIR = REPO / "apps" / "api"
sys.path.insert(0, str(API_DIR))
OUT_DIR = REPO / "contracts" / "golden"

import django  # noqa: E402
from django.conf import settings  # noqa: E402

settings.configure(
    DEBUG=False,
    ALLOWED_HOSTS=["*"],
    SECRET_KEY="contract-capture",
    ROOT_URLCONF=__name__,
    DEFAULT_CHARSET="utf-8",
    # Plane's `settings/common.py` ties the body budget to `FILE_SIZE_LIMIT`;
    # spell every limit out so a change in Django's defaults cannot silently
    # move the golden set.
    DATA_UPLOAD_MAX_MEMORY_SIZE=5_242_880,
    DATA_UPLOAD_MAX_NUMBER_FIELDS=1_000,
    DATA_UPLOAD_MAX_NUMBER_FILES=100,
    INSTALLED_APPS=["django.contrib.contenttypes", "django.contrib.auth", "rest_framework"],
    REST_FRAMEWORK={
        "DEFAULT_AUTHENTICATION_CLASSES": (),
        "DEFAULT_PERMISSION_CLASSES": ("rest_framework.permissions.AllowAny",),
        "DEFAULT_RENDERER_CLASSES": ("rest_framework.renderers.JSONRenderer",),
    },
)
django.setup()

from django.core.files.uploadedfile import SimpleUploadedFile  # noqa: E402
from django.http import QueryDict  # noqa: E402
from django.test import Client  # noqa: E402
from django.test.client import BOUNDARY, FakePayload, encode_multipart  # noqa: E402
from django.urls import path  # noqa: E402
from rest_framework import serializers  # noqa: E402
from rest_framework.utils import json as drf_json  # noqa: E402
from rest_framework.parsers import (  # noqa: E402
    FormParser,
    JSONParser,
    MultiPartParser,
)
from rest_framework.response import Response  # noqa: E402
from rest_framework.views import APIView  # noqa: E402

class Payload(serializers.Serializer):
    name = serializers.CharField()

class BodyView(APIView):
    permission_classes = []
    parser_classes = [JSONParser, FormParser, MultiPartParser]

    def post(self, request):
        serializer = Payload(data=request.data)
        serializer.is_valid(raise_exception=True)
        return Response({"accepted": serializer.validated_data})

class RawView(APIView):
    """Echo what the parser produced, in a shape a Rust parser can reproduce.

    Form bodies are reported as Django splits them (`request.POST` fields and
    `request.FILES` metadata) instead of the merged `request.data`, because the
    merge replaces a repeated field with its last value and hides the file list.
    File bytes are summarised by size, never echoed.
    """

    permission_classes = []
    parser_classes = [JSONParser, FormParser, MultiPartParser]

    def post(self, request):
        is_form = request.content_type.startswith(
            ("application/x-www-form-urlencoded", "multipart/form-data")
        )
        files = [
            {
                "name": key,
                "filename": uploaded.name,
                "content_type": uploaded.content_type,
                "size": uploaded.size,
            }
            for key in sorted(request.FILES)
            for uploaded in request.FILES.getlist(key)
        ]
        if is_form:
            data = {key: request.POST.getlist(key) for key in request.POST}
            return Response({"kind": "form", "data": data, "files": files})
        return Response({"kind": "json", "data": request.data, "files": files})

urlpatterns = [
    path("api/parser-body/", BodyView.as_view()),
    path("api/parser-raw/", RawView.as_view()),
]

UA = "Mozilla/5.0 (compatible; planesync/1.0)"
KEPT_HEADERS = ("content-type", "allow", "location")

def record(name, method, path_, content_type, body, extra_headers=None):
    # Everything travels as raw bytes through the WSGI environ: `Client.post`
    # re-encodes with the charset from the header, which would erase the very
    # cases this capture is about (utf-16, invalid utf-8, absent Content-Type).
    # Dicts are encoded here so the multipart boundary is visible in the header
    # and `request.body` is stored for every case.
    if isinstance(body, dict):
        body = encode_multipart(BOUNDARY, body)
        if content_type is None:
            content_type = f"multipart/form-data; boundary={BOUNDARY}"
    environ = {
        "PATH_INFO": path_,
        "REQUEST_METHOD": method,
        "SERVER_PORT": "80",
        "wsgi.url_scheme": "http",
        "CONTENT_LENGTH": str(len(body)),
        "wsgi.input": FakePayload(body),
        "HTTP_USER_AGENT": UA,
        "HTTP_ACCEPT": "application/json",
    }
    if content_type is not None:
        environ["CONTENT_TYPE"] = content_type
    if extra_headers:
        environ.update(extra_headers)
    # A lone surrogate parses but cannot be re-encoded, so DRF fails while
    # rendering. Recording that 500 is the point of the case.
    response = Client(raise_request_exception=False).request(**environ)

    directory = OUT_DIR / name
    directory.mkdir(parents=True, exist_ok=True)

    request_lines = [
        f"{method} {path_} HTTP/1.1",
        "Host: 127.0.0.1:8000",
        f"User-Agent: {UA}",
        "Accept: application/json",
    ]
    if content_type is not None:
        request_lines.append(f"Content-Type: {content_type}")
    request_lines.append("Cookie: <session cookie from live sign-in>")
    request_lines.append("")
    (directory / "request.http").write_text(
        "\n".join(request_lines) + "\n", encoding="utf-8", newline=""
    )

    kept = sorted(
        (key.title(), value)
        for key, value in response.items()
        if key.lower() in KEPT_HEADERS
    )
    response_lines = [f"HTTP/1.1 {response.status_code}"]
    response_lines += [f"{key}: {value}" for key, value in kept]
    (directory / "response.http").write_text(
        "\n".join(response_lines) + "\n\n", encoding="utf-8", newline=""
    )

    for stale in ("body.json", "body.raw"):
        target = directory / stale
        if target.exists():
            target.unlink()
    (directory / "body.raw").write_bytes(response.content)
    try:
        parsed = json.loads(response.content)
    except ValueError:
        pass
    else:
        (directory / "body.json").write_text(
            json.dumps(parsed, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
            newline="",
        )

    (directory / "request.body").write_bytes(bytes(body))
    print(f"{name}: HTTP {response.status_code} {response.content[:110]!r}")

def main():
    raw = "/api/parser-raw/"
    body = "/api/parser-body/"

    record("parser-json-object", "POST", raw, "application/json",
           b'{"name":"Ren\xc3\xa9 \xe4\xbd\xa0\xe5\xa5\xbd","n":1,"ok":true,"nil":null,"list":[1,2]}')
    record("parser-json-array", "POST", raw, "application/json", b"[1,2,3]")
    record("parser-json-scalar", "POST", raw, "application/json", b'42')
    record("parser-json-duplicate-keys", "POST", raw, "application/json", b'{"a":1,"a":2}')
    record("parser-json-big-int", "POST", raw, "application/json",
           b'{"n":123456789012345678901234567890}')
    record("parser-json-big-float", "POST", raw, "application/json",
           b'{"n":212153.88871970866}')
    record("parser-json-float-forms", "POST", raw, "application/json",
           b'{"a":1e16,"b":1E-5,"c":-0.0,"d":1.0}')
    record("parser-json-unknown-field", "POST", body, "application/json",
           b'{"name":"x","extra":1}')
    record("parser-json-charset", "POST", raw, "application/json; charset=utf-8",
           b'{"name":"Ren\xc3\xa9"}')
    record("parser-json-utf16", "POST", raw, "application/json; charset=utf-16",
           '{"name":"Ren\u00e9"}'.encode("utf-16"))
    record("parser-json-unknown-charset", "POST", raw, 'application/json; charset=nope',
           b'{"name":"x"}')
    record("parser-json-nontext-codec", "POST", raw, 'application/json; charset=bz2',
           b'{"name":"x"}')
    record("parser-json-empty", "POST", raw, "application/json", b"")
    record("parser-json-trailing", "POST", raw, "application/json", b'{"a":1}x')
    record("parser-json-single-quotes", "POST", raw, "application/json", b"{'a':1}")
    record("parser-json-nan", "POST", raw, "application/json", b'{"n":NaN}')
    record("parser-json-infinity", "POST", raw, "application/json", b'{"n":Infinity}')
    record("parser-json-control-char", "POST", raw, "application/json", b'{"a":"x\x01y"}')
    record("parser-json-bad-utf8", "POST", raw, "application/json", b'{"a":"\xff"}')
    record("parser-json-unterminated", "POST", raw, "application/json", b'{"a":"x}')
    record("parser-json-bad-escape", "POST", raw, "application/json", b'{"a":"\\q"}')
    record("parser-json-bad-unicode-escape", "POST", raw, "application/json", b'{"a":"\\u00zz"}')
    record("parser-json-missing-colon", "POST", raw, "application/json", b'{"a" 1}')
    record("parser-json-missing-comma", "POST", raw, "application/json", b'{"a":1 "b":2}')
    record("parser-415-text-plain", "POST", raw, "text/plain", b'{"a":1}')
    record("parser-415-no-content-type", "POST", raw, None, b'{"a":1}')
    record("parser-415-xml", "POST", raw, "application/xml", b"<a/>")
    record("parser-415-empty-header", "POST", raw, "", b'{"a":1}')
    record("parser-form", "POST", raw, "application/x-www-form-urlencoded",
           b"name=Ren%C3%A9&dup=1&dup=2")
    record("parser-form-array", "POST", raw, "application/x-www-form-urlencoded",
           b"items=a&items=b&single=c")
    record("parser-form-unicode", "POST", raw,
           "application/x-www-form-urlencoded; charset=utf-8",
           "name=Ren\u00e9".encode("utf-8"))
    record("parser-form-empty", "POST", raw, "application/x-www-form-urlencoded", b"")
    record("parser-multipart", "POST", raw, None,
           {"name": "Ren\u00e9", "note": "two"})
    record("parser-multipart-file", "POST", raw, None,
           {"name": "x",
            "upload": SimpleUploadedFile("a.txt", b"hello", content_type="text/plain")})
    record("parser-multipart-body", "POST", body, None, {"name": "x", "extra": "ignored"})

    # Empty bodies never reach a parser: `Request._parse` short-circuits before
    # selection, so even `text/plain` answers 200 `{}` here. This is the quirk
    # that makes a blanket "unsupported type -> 415" wrong.
    record("parser-empty-text-plain", "POST", raw, "text/plain", b"")
    record("parser-empty-no-content-type", "POST", raw, None, b"")
    record("parser-empty-json", "POST", raw, "application/json", b"")
    record("parser-empty-json-bad-charset", "POST", raw,
           "application/json; charset=nope", b"")

    # DRF's FormParser decodes with the charset from the header, unlike Django's
    # own POST path (which insists on UTF-8).
    record("parser-form-latin1", "POST", raw,
           "application/x-www-form-urlencoded; charset=iso-8859-1",
           "name=Ren\u00e9".encode("iso-8859-1"))
    record("parser-form-bad-percent", "POST", raw,
           "application/x-www-form-urlencoded", b"a=%zz&b=%")
    record("parser-form-plus", "POST", raw,
           "application/x-www-form-urlencoded", b"a=1+2&b=%2B3")

    # A quoted boundary and a part with an RFC 5987 filename exercise header
    # parsing, not just the delimiter scan.
    boundary = "----planeBoundary42"
    quoted = (
        f"--{boundary}\r\n"
        'Content-Disposition: form-data; name="note"\r\n\r\n'
        "hello\r\n"
        f"--{boundary}\r\n"
        'Content-Disposition: form-data; name="upload"; filename="r\u00e9sum\u00e9.txt"\r\n'
        "Content-Type: text/plain\r\n\r\n"
        "bye\r\n"
        f"--{boundary}--\r\n"
    ).encode("utf-8")
    record("parser-multipart-quoted-boundary", "POST", raw,
           f'multipart/form-data; boundary="{boundary}"', quoted)

    record("parser-json-bom", "POST", raw, "application/json",
           b"\xef\xbb\xbf{\"a\":1}")
    record("parser-json-long-media-type", "POST", raw,
           "application/json; " + "x" * 300, b'{"a":1}')
    record("parser-json-charset-case", "POST", raw,
           "Application/JSON; Charset=UTF-8", b'{"a":1}')
    record("parser-json-short-unicode", "POST", raw, "application/json",
           b'{"a":"\\ud83d"}')
    record("parser-json-surrogate-pair", "POST", raw, "application/json",
           b'{"a":"\\ud83d\\ude00"}')

JSON_ERROR_INPUTS = [
    b"",
    b" ",
    b"{",
    b"[",
    b'{"a":',
    b'{"a":1',
    b"[1,",
    b"[1,]",
    b"{,}",
    b'{"a":1,}',
    b"tru",
    b"nul",
    b"fals",
    b'"abc',
    b"01",
    b"+1",
    b".5",
    b"1.",
    b"-",
    b"--1",
    b"1e",
    b"1e+",
    b"\n\n  {\"a\" 1}",
    b'{"a":\n1 "b":2}',
    b'\t{"a":\t"\x01"}',
    b'{"a":"x\x01y"}',
    b'{"a":"\\q"}',
    b'{"a":"\\u00zz"}',
    b'{"a":1}x',
    b'{"a":1} x',
    b'{"a":1}   ',
    b"[1]2",
    b"{}[]",
    b'{"a":1,"b":}',
    b"{'a':1}",
    b'{"a" 1}',
    b'{"a":1 "b":2}',
    b"[]]",
    b"]]",
    b"1 2",
    b'{"a":tru}',
    b'{"a":nul}',
    b'{"a":fals}',
    b'{"a":+1}',
    b'{"a":.5}',
    b'{"a":01}',
    b'{"a":1.}',
    b'{"a":1e}',
    b'{"a":1e+}',
    b'{"a":-}',
    b'{"a":"\\u12"}',
    b'{"a":"\\u"}',
    b'{"a":"\\ud83d"}',
    b'{"a":"\\ud83d\\ude00"}',
    b'{"a":"\\ud83dx"}',
]


def write_json_error_table():
    """Freeze CPython's decoder messages, which DRF echoes inside a 400."""

    table = []
    for raw in JSON_ERROR_INPUTS:
        text = raw.decode("utf-8")
        try:
            drf_json.loads(text)
        except ValueError as exc:
            table.append(
                {
                    "input": text,
                    "error": str(exc),
                    "kind": type(exc).__name__,
                }
            )
        else:
            table.append({"input": text, "error": None, "kind": "ok"})
    (OUT_DIR / "parser-json-errors.json").write_text(
        json.dumps(table, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
        newline="",
    )
    errors = sum(1 for row in table if row["error"])
    print(f"parser-json-errors.json: {len(table)} inputs, {errors} errors")


def digest() -> str:
    """Hash every recorded parser file, sorted by path (bytes included)."""

    import hashlib

    records = []
    for path in sorted(OUT_DIR.glob("parser-*")):
        if path.is_dir():
            records.extend(sorted(p for p in path.iterdir() if p.is_file()))
        elif path.is_file():
            records.append(path)
    hasher = hashlib.sha256()
    for path in records:
        hasher.update(path.relative_to(OUT_DIR).as_posix().encode())
        hasher.update(b"\0")
        hasher.update(path.read_bytes())
        hasher.update(b"\0")
    return hasher.hexdigest()


if __name__ == "__main__":
    main()
    write_json_error_table()
    first = digest()
    main()
    write_json_error_table()
    second = digest()
    if first != second:
        raise SystemExit(f"capture is not idempotent: {first} != {second}")
    print(f"parser-*: stable ({first})")
