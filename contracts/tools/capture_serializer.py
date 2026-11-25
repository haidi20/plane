# Copyright (c) 2023-present Plane Software, Inc. and contributors
# SPDX-License-Identifier: AGPL-3.0-only
# See the LICENSE file for details.

"""Record the wire bytes of DRF's field serializers, without Docker.

The card is about *formatting*, so the oracle is DRF's own field classes driven
by a plain ``Serializer`` (no model, no DB). That keeps the capture honest: the
bytes come from ``JSONRenderer``, the same encoder every ``/api`` response uses.

Run from the repo root with the ``apps/api`` venv:

    apps\\api\\.venv\\Scripts\\python.exe contracts\\tools\\capture_serializer.py

Cases written under ``contracts/golden/serializer-*``.
"""

import datetime
import json
import pathlib
import random
import sys
import uuid

REPO = pathlib.Path(__file__).resolve().parents[2]
API_DIR = REPO / "apps" / "api"
sys.path.insert(0, str(API_DIR))
OUT_DIR = REPO / "contracts" / "golden"

import django  # noqa: E402
from django.conf import settings  # noqa: E402

settings.configure(
    DEBUG=False,
    USE_TZ=True,
    TIME_ZONE="UTC",
    SECRET_KEY="contract-capture",
    INSTALLED_APPS=["django.contrib.contenttypes", "django.contrib.auth", "rest_framework"],
    REST_FRAMEWORK={"DEFAULT_RENDERER_CLASSES": ("rest_framework.renderers.JSONRenderer",)},
)
django.setup()

from django.utils import timezone  # noqa: E402
from rest_framework import serializers  # noqa: E402
from rest_framework.renderers import JSONRenderer  # noqa: E402

UTC = datetime.timezone.utc
INSTANT = datetime.datetime(2026, 11, 24, 17, 21, 52, 123759, tzinfo=UTC)

class Recorder(serializers.Serializer):
    """One field per formatting rule the card lists."""

    id = serializers.UUIDField()
    created_at = serializers.DateTimeField()
    updated_at = serializers.DateTimeField()
    start_date = serializers.DateField(allow_null=True)
    estimate = serializers.FloatField(allow_null=True)
    sort_order = serializers.FloatField()
    tiny = serializers.FloatField()
    big = serializers.FloatField()
    name = serializers.CharField(allow_null=True)
    description = serializers.CharField(allow_null=True)
    missing = serializers.CharField(required=False)

def record(name, payload):
    """Write one case in the same shape as the other captures."""

    data = Recorder(payload).data
    body = JSONRenderer().render(data)
    directory = OUT_DIR / name
    directory.mkdir(parents=True, exist_ok=True)

    (directory / "body.raw").write_bytes(body)
    (directory / "body.json").write_text(
        json.dumps(json.loads(body), indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="",
    )
    (directory / "request.http").write_text(
        "\n".join(
            [
                "GET /api/serializer-fixture/ HTTP/1.1",
                "Host: 127.0.0.1:8000",
                "User-Agent: Mozilla/5.0 (compatible; planesync/1.0)",
                "Accept: application/json",
                "Cookie: <session cookie from live sign-in>",
                "",
            ]
        )
        + "\n",
        encoding="utf-8",
        newline="",
    )
    (directory / "response.http").write_text(
        "HTTP/1.1 200\nContent-Type: application/json\n\n", encoding="utf-8", newline=""
    )
    print(f"{name}: {body!r}")

def payload(**overrides):
    base = {
        "id": uuid.UUID("DEB5350A-F8C9-4CD5-9875-5323DC6CAB2A"),
        "created_at": INSTANT,
        "updated_at": INSTANT,
        "start_date": datetime.date(2026, 1, 2),
        "estimate": 3.5,
        "sort_order": 65535.0,
        "tiny": 1e-7,
        "big": 1e16,
        "name": "Ren\u00e9 \u2014 \u4f60\u597d",
        "description": None,
    }
    base.update(overrides)
    return base

def float_case_table():
    """Deterministic `repr()` table for the Rust formatter.

    `repr(float)` is the contract, so the table mixes hand-picked edges
    (exponents, `-0.0`, subnormals) with a fixed-seed sample. The seed keeps the
    file stable across runs: re-running the capture must not touch it.
    """

    picked = [
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        2.0,
        10.0,
        100.0,
        1e-3,
        1e-4,
        1e-5,
        1e-6,
        1e-7,
        1e-10,
        1e15,
        1e16,
        1e17,
        1e300,
        1.7976931348623157e308,
        5e-324,
        2.2250738585072014e-308,
        1.0 / 3.0,
        -1.0 / 3.0,
        0.1,
        0.2,
        0.30000000000000004,
        123456789.123456789,
        1234567890.1234567,
        65535.0,
        3.5,
        212153.88871970866,
        -875937.0,
        58790.0,
        216848.0,
    ]
    rng = random.Random(20261125)
    print_rng = random.Random(4242)
    generated = []
    while len(generated) < 184:
        mode = rng.randrange(4)
        if mode == 0:
            value = rng.uniform(-1e6, 1e6)
        elif mode == 1:
            value = rng.uniform(-1.0, 1.0)
        elif mode == 2:
            value = rng.uniform(-1e-6, 1e-6)
        else:
            value = print_rng.choice(
                [rng.uniform(-1e19, 1e19), rng.uniform(-1e-9, 1e-9)]
            )
        generated.append(value)
    return [{"value": value, "repr": repr(value)} for value in picked + generated]


record("serializer-fields", payload())
# Django's `TimezoneMixin` activates the *user's* timezone, so an offset other
# than UTC must render `+07:00`, not `Z`. This case proves the formatter cannot
# hardcode `Z`.
timezone.activate("Asia/Jakarta")
record("serializer-fields-jakarta", payload())
timezone.deactivate()
record(
    "serializer-fields-no-micros",
    payload(created_at=INSTANT.replace(microsecond=0), updated_at=INSTANT.replace(microsecond=1000)),
)
record(
    "serializer-fields-nulls",
    payload(start_date=None, estimate=None, name=None, description=None),
)
record(
    "serializer-fields-floats",
    payload(
        created_at=INSTANT,
        sort_order=0.0,
        tiny=1e-5,
        big=1.7976931348623157e308,
    ),
)


# Written last so the case directories above are already on disk if this fails.
float_cases = float_case_table()
(OUT_DIR / "serializer-float-cases.json").write_text(
    json.dumps(float_cases, indent=2, ensure_ascii=False) + "\n",
    encoding="utf-8",
    newline="",
)
print(f"serializer-float-cases.json: {len(float_cases)} cases")
