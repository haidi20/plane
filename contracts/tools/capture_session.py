# Copyright (c) 2023-present Plane Software, Inc. and contributors
# SPDX-License-Identifier: AGPL-3.0-only
# See the LICENSE file for details.

"""Record Plane's Django session cookie contract without importing Django.

The oracle is the *text* of the Django installed in ``apps/api/.venv`` plus the
Plane source tree:

* ``contrib/sessions/backends/base.py`` -> ``SessionBase`` (encode/decode,
  ``key_salt``, expiry rules) and ``VALID_KEY_CHARS``;
* ``contrib/sessions/backends/db.py`` -> ``SessionStore.create_model_instance``;
* ``core/signing.py`` -> ``Signer``/``TimestampSigner``/``dumps``/``loads``,
  base62 + base64 helpers, ``JSONSerializer``;
* ``utils/crypto.py`` -> ``salted_hmac``, ``get_random_string``,
  ``constant_time_compare``, ``RANDOM_STRING_CHARS``;
* ``conf/global_settings.py`` -> the defaults Plane does not override
  (``SESSION_SERIALIZER``, ``SESSION_EXPIRE_AT_BROWSER_CLOSE``, ``USE_TZ``);
* ``plane/settings/common.py`` -> the ``SESSION_*``/``ADMIN_SESSION_*`` values;
* ``plane/db/models/session.py`` -> plane's ``SessionStore`` override
  (128-character key, ``user_id``/``device_info`` mapping);
* ``plane/db/migrations/0065_auto_20240415_0937.py`` -> the ``sessions`` table.

Everything runs in a bare namespace. Four names are re-typed because Django
only reaches them through modules that need ``settings`` at import time:

* ``force_bytes`` -> ``s.encode("utf-8")`` (every capture input is ``str``);
* ``_lazy_re_compile`` -> ``re.compile`` (the separator guard);
* ``time`` -> a module-like object pinned to one second, so ``TimestampSigner``
  is deterministic;
* ``get_random_string`` -> a deterministic filler that *records* the requested
  length and alphabet, which is the datum we want (the characters are not).

No Django import, no database, no container. Run from the repo root with the
interpreter the API targets (``apps/api/pyproject.toml`` says
``>=3.12,<3.13``):

    apps\\api\\.venv\\Scripts\\python.exe contracts\\tools\\capture_session.py

The output is ``contracts/golden/session-cases.json``; re-running it must not
change a byte. ``cargo test -p plane-pkg --test session_golden`` replays it.
"""

import ast
import base64
import datetime
import hashlib
import hmac as hmac_module
import json
import pathlib
import re
import secrets
import sys
import types
import zlib

REPO = pathlib.Path(__file__).resolve().parents[2]
VENV = REPO / "apps" / "api" / ".venv"
OUT = REPO / "contracts" / "golden" / "session-cases.json"
SITE_DIRS = ("Lib/site-packages", "lib/python3.12/site-packages")


def site_packages():
    for relative in SITE_DIRS:
        candidate = VENV / relative
        if (candidate / "django").is_dir():
            return candidate
    raise SystemExit(f"django is not installed under {VENV}")


SITE = site_packages()
CRYPTO = SITE / "django" / "utils" / "crypto.py"
SIGNING = SITE / "django" / "core" / "signing.py"
SESSIONS_BASE = SITE / "django" / "contrib" / "sessions" / "backends" / "base.py"
SESSIONS_DB = SITE / "django" / "contrib" / "sessions" / "backends" / "db.py"
GLOBALS = SITE / "django" / "conf" / "global_settings.py"
PLANE_SETTINGS = REPO / "apps" / "api" / "plane" / "settings" / "common.py"
PLANE_SESSION = REPO / "apps" / "api" / "plane" / "db" / "models" / "session.py"
PLANE_MIGRATION = (
    REPO / "apps" / "api" / "plane" / "db" / "migrations" / "0065_auto_20240415_0937.py"
)

CLOCK = datetime.datetime(2026, 11, 27, 12, 0, 0, tzinfo=datetime.timezone.utc)
TIMESTAMP = int(CLOCK.timestamp())
SECRET_KEY = "capture-only-secret-key-0123456789abcdefghijklmnop"
OTHER_SECRET = "capture-only-secret-key-fedcba9876543210zyxwvutsrqponm"


def django_version():
    for metadata in SITE.glob("django-*.dist-info/METADATA"):
        for line in metadata.read_text(encoding="utf-8").splitlines():
            if line.startswith("Version: "):
                return line.split(": ", 1)[1].strip()
    raise SystemExit("django dist-info not found; cannot stamp the version")


def module_level(path, names):
    """Return the module-level functions and classes named in *names*, in order."""
    tree = ast.parse(path.read_text(encoding="utf-8"))
    wanted = set(names)
    found = {}
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.ClassDef)) and node.name in wanted:
            found[node.name] = node
    missing = wanted - set(found)
    if missing:
        raise SystemExit(f"{path.name}: moved or renamed: {sorted(missing)}")
    return tree, [found[name] for name in names]


def module_method(path, class_name, method_name):
    """Return one ``FunctionDef`` out of a class, without executing the class."""
    tree, _ = module_level(path, (class_name,))
    for node in tree.body:
        if isinstance(node, ast.ClassDef) and node.name == class_name:
            for item in node.body:
                if isinstance(item, ast.FunctionDef) and item.name == method_name:
                    return item
    raise SystemExit(f"{path.name}: {class_name}.{method_name} not found")


def module_constant(path, name):
    """Read a module-level ``NAME = <literal>`` without executing the module."""
    tree = ast.parse(path.read_text(encoding="utf-8"))
    for node in tree.body:
        if not isinstance(node, ast.Assign):
            continue
        for target in node.targets:
            if isinstance(target, ast.Name) and target.id == name:
                return ast.literal_eval(node.value)
    raise SystemExit(f"{path.name}: {name} not found")


def module_assignment_names(path):
    names = set()
    tree = ast.parse(path.read_text(encoding="utf-8"))
    for node in tree.body:
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name):
                    names.add(target.id)
    return names


def exec_nodes(nodes, namespace, origin):
    module = ast.Module(body=list(nodes), type_ignores=[])
    exec(compile(module, str(origin), "exec"), namespace)


def synth_class(name, bases, body):
    """Build a ``ClassDef`` out of already-parsed members."""
    node = ast.ClassDef(
        name=name,
        bases=[ast.Name(id=base, ctx=ast.Load()) for base in bases],
        keywords=[],
        body=list(body),
        decorator_list=[],
    )
    ast.fix_missing_locations(node)
    return node


def assert_assignments(path, wanted):
    """Fail loudly when a settings module stops defining a value we capture."""
    present = module_assignment_names(path)
    missing = set(wanted) - present
    if missing:
        raise SystemExit(f"{path.name}: settings moved or renamed: {sorted(missing)}")


def plane_session_settings():
    """Execute the ``SESSION_*`` assignments of ``plane/settings/common.py``.

    ``os.environ`` is shimmed so every ``os.environ.get(name, default)`` yields
    the shipped default; that is the configuration a fresh deployment runs.
    """
    wanted = (
        "SESSION_COOKIE_HTTPONLY",
        "SESSION_COOKIE_SECURE",
        "SESSION_ENGINE",
        "SESSION_COOKIE_AGE",
        "SESSION_COOKIE_NAME",
        "SESSION_COOKIE_DOMAIN",
        "SESSION_SAVE_EVERY_REQUEST",
        "ADMIN_SESSION_COOKIE_NAME",
        "ADMIN_SESSION_COOKIE_AGE",
    )
    assert_assignments(PLANE_SETTINGS, wanted)

    class EmptyEnvironment(dict):
        def get(self, key, default=None):
            return default

    tree = ast.parse(PLANE_SETTINGS.read_text(encoding="utf-8"))
    nodes = []
    for node in tree.body:
        if not isinstance(node, ast.Assign):
            continue
        targets = {
            target.id for target in node.targets if isinstance(target, ast.Name)
        }
        if targets & set(wanted):
            nodes.append(node)
    namespace = {
        "os": types.SimpleNamespace(environ=EmptyEnvironment()),
        "int": int,
        # ``SESSION_COOKIE_SECURE = _cookies_secure(..., cors_allowed_origins, ...)``
        # needs both names in scope; an empty CORS list is the shipped default and
        # makes the cookie flag False, which is all this oracle needs.
        "cors_allowed_origins": [],
    }
    _, helper_nodes = module_level(PLANE_SETTINGS, ("_cookies_secure",))
    exec_nodes(helper_nodes, namespace, PLANE_SETTINGS)
    exec_nodes(nodes, namespace, PLANE_SETTINGS)
    return {name: namespace[name] for name in wanted}


def build_signing(secret=SECRET_KEY):
    """``Signer``/``TimestampSigner``/``dumps``/``loads`` from Django's source."""
    sep_namespace = {"_lazy_re_compile": re.compile}
    exec_nodes([module_assign(SIGNING, "_SEP_UNSAFE")], sep_namespace, SIGNING)
    namespace = {
        "_SEP_UNSAFE": sep_namespace["_SEP_UNSAFE"],
        "base64": base64,
        "datetime": datetime,
        "json": json,
        "time": types.SimpleNamespace(time=lambda: float(TIMESTAMP)),
        "zlib": zlib,
        "re": re,
        "_lazy_re_compile": re.compile,
        "BASE62_ALPHABET": module_constant(SIGNING, "BASE62_ALPHABET"),
        "force_bytes": lambda value: value.encode("utf-8")
        if isinstance(value, str)
        else bytes(value),
        "settings": types.SimpleNamespace(
            SECRET_KEY=secret, SECRET_KEY_FALLBACKS=[], SIGNING_BACKEND=""
        ),
    }
    _, crypto_nodes = module_level(CRYPTO, ("salted_hmac", "constant_time_compare"))
    exec_nodes(crypto_nodes, namespace, CRYPTO)
    _, signing_nodes = module_level(
        SIGNING,
        (
            "BadSignature",
            "SignatureExpired",
            "b62_encode",
            "b62_decode",
            "b64_encode",
            "b64_decode",
            "base64_hmac",
            "JSONSerializer",
            "Signer",
            "TimestampSigner",
            "dumps",
            "loads",
        ),
    )
    exec_nodes(signing_nodes, namespace, SIGNING)
    return namespace


def module_assign(path, name):
    tree = ast.parse(path.read_text(encoding="utf-8"))
    for node in tree.body:
        if not isinstance(node, ast.Assign):
            continue
        for target in node.targets:
            if isinstance(target, ast.Name) and target.id == name:
                return node
    raise SystemExit(f"{path.name}: assignment {name} not found")


class RandomRecorder:
    """Deterministic ``get_random_string`` that records what was asked for."""

    def __init__(self):
        self.calls = []

    def __call__(self, length, allowed_chars=None):
        self.calls.append({"length": length, "alphabet": allowed_chars})
        alphabet = allowed_chars or "abc"
        return "".join(alphabet[index % len(alphabet)] for index in range(length))


def build_session_base(signing, settings, recorder, serializer_path):
    if serializer_path != "django.contrib.sessions.serializers.JSONSerializer":
        raise SystemExit(f"unexpected SESSION_SERIALIZER: {serializer_path}")

    def import_string(path):
        if path != serializer_path:
            raise SystemExit(f"capture only knows {serializer_path}, got {path}")
        return signing["JSONSerializer"]

    namespace = {
        "string": __import__("string"),
        "datetime": datetime,
        "timedelta": datetime.timedelta,
        "settings": types.SimpleNamespace(
            SESSION_SERIALIZER=serializer_path,
            SESSION_COOKIE_AGE=settings["SESSION_COOKIE_AGE"],
            SESSION_EXPIRE_AT_BROWSER_CLOSE=False,
        ),
        "signing": types.SimpleNamespace(
            dumps=signing["dumps"],
            loads=signing["loads"],
            BadSignature=signing["BadSignature"],
            SignatureExpired=signing["SignatureExpired"],
        ),
        "timezone": types.SimpleNamespace(now=lambda: CLOCK),
        "get_random_string": recorder,
        "import_string": import_string,
        "sync_to_async": lambda function: function,
    }
    _, session_nodes = module_level(SESSIONS_BASE, ("SessionBase",))
    exec_nodes(
        [module_assign(SESSIONS_BASE, "VALID_KEY_CHARS"), *session_nodes],
        namespace,
        SESSIONS_BASE,
    )
    return namespace


class ModelFactory:
    """Stand-in for the ``sessions`` model instance Django would build."""

    def __init__(self, **fields):
        self.fields = fields

    def __repr__(self):
        return f"ModelFactory({self.fields})"


def build_plane_store(base_namespace):
    namespace = dict(base_namespace)
    namespace["Session"] = ModelFactory
    exec_nodes(
        [
            module_assign(PLANE_SESSION, "VALID_KEY_CHARS"),
            synth_class(
                "DjangoDbStore",
                ["SessionBase"],
                [module_method(SESSIONS_DB, "SessionStore", "create_model_instance")],
            ),
            synth_class(
                "SessionStore",
                ["DjangoDbStore"],
                [
                    module_method(PLANE_SESSION, "SessionStore", "_get_new_session_key"),
                    module_method(
                        PLANE_SESSION, "SessionStore", "create_model_instance"
                    ),
                ],
            ),
        ],
        namespace,
        PLANE_SESSION,
    )
    store_class = namespace["SessionStore"]
    store_class.model = staticmethod(ModelFactory)
    return namespace, store_class


def sessions_table():
    tree = ast.parse(PLANE_MIGRATION.read_text(encoding="utf-8"))
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        if not isinstance(node.func, ast.Attribute) or node.func.attr != "CreateModel":
            continue
        keywords = {keyword.arg: keyword.value for keyword in node.keywords}
        name = keywords.get("name")
        if not isinstance(name, ast.Constant) or name.value != "Session":
            continue
        return {
            "name": "Session",
            "fields": [ast.unparse(element) for element in keywords["fields"].elts],
            "options": ast.unparse(keywords["options"]),
        }
    raise SystemExit("CreateModel(name='Session') not found in the migration")


LOGIN_MINIMAL = {
    "_auth_user_id": "6f0d1f8e-3f8e-4a1e-9f6a-1c2d3e4f5a6b",
    "_auth_user_backend": "django.contrib.auth.backends.ModelBackend",
    "_auth_user_hash": "8c1f0d1f8e3f8e4a1e9f6a1c2d3e4f5a6b7c8d9e",
}

LOGIN_DEVICE = {
    **LOGIN_MINIMAL,
    "device_info": {
        "device_id": "9c1f2f7a-1b2c-4d5e-8f90-1234567890ab",
        "device_name": "Chrome on macOS",
        "city": "München",
        "ip": "1.2.3.4",
    },
}

ENCODE_PAYLOADS = [
    ("empty", {}),
    ("oauth-state", {"state": "z1x2c3v4b5n6m7q8"}),
    ("login-minimal", LOGIN_MINIMAL),
    ("login-device-unicode", LOGIN_DEVICE),
    (
        "nested-scalars",
        {"a": [1, 2.5, True, None], "b": {"c": "ü"}, "d": 1e-05, "e": 1234567890},
    ),
    ("session-expiry", {**LOGIN_MINIMAL, "_session_expiry": 1200}),
    ("long-compressed", {"data": "x" * 3000}),
]


def main():
    settings = plane_session_settings()
    defaults = {
        "SESSION_SERIALIZER": module_constant(GLOBALS, "SESSION_SERIALIZER"),
        "SESSION_EXPIRE_AT_BROWSER_CLOSE": module_constant(
            GLOBALS, "SESSION_EXPIRE_AT_BROWSER_CLOSE"
        ),
        "USE_TZ": module_constant(GLOBALS, "USE_TZ"),
    }
    signing = build_signing()
    recorder = RandomRecorder()
    base_namespace = build_session_base(
        signing, settings, recorder, defaults["SESSION_SERIALIZER"]
    )
    _, store_class = build_plane_store(base_namespace)
    serializer = signing["JSONSerializer"]()
    store = store_class()
    key_salt = store.key_salt

    encode_rows = []
    for label, payload in ENCODE_PAYLOADS:
        session_data = store.encode(payload)
        payload_b64, timestamp, signature = session_data.rsplit(":", 2)
        raw = serializer.dumps(payload)
        compressed = zlib.compress(raw)
        is_compressed = len(compressed) < len(raw) - 1
        if payload_b64.startswith(".") != is_compressed:
            raise SystemExit(f"{label}: compression flag does not follow the rule")
        row = {
            "case": label,
            "payload": payload,
            "json": raw.decode("latin-1"),
            "json_bytes": len(raw),
            "compressed": is_compressed,
            "session_data": session_data,
            "payload_b64": payload_b64,
            "timestamp": timestamp,
            "signature": signature,
            "decoded": store.decode(session_data),
        }
        if is_compressed:
            row["zlib_hex"] = compressed.hex()
        encode_rows.append(row)

    valid = encode_rows[2]["session_data"]
    compressed_valid = encode_rows[6]["session_data"]
    payload_b64, timestamp, signature = valid.rsplit(":", 2)
    tampered = valid[:-1] + ("A" if valid[-1] != "A" else "B")
    edited_payload = payload_b64 + "x:" + timestamp + ":" + signature
    other_store = build_plane_store(
        build_session_base(
            build_signing(OTHER_SECRET),
            settings,
            RandomRecorder(),
            defaults["SESSION_SERIALIZER"],
        )
    )[1]
    decode_rows = [
        {"case": "valid", "session_data": valid, "decoded": store.decode(valid)},
        {
            "case": "valid-compressed",
            "session_data": compressed_valid,
            "decoded": store.decode(compressed_valid),
        },
        {
            "case": "tampered-signature",
            "session_data": tampered,
            "decoded": store.decode(tampered),
        },
        {
            "case": "wrong-secret",
            "session_data": valid,
            "decoded": other_store().decode(valid),
        },
        {
            "case": "payload-edited",
            "session_data": edited_payload,
            "decoded": store.decode(edited_payload),
        },
        {
            "case": "no-separator",
            "session_data": "not-a-session",
            "decoded": store.decode("not-a-session"),
        },
        {"case": "empty", "session_data": "", "decoded": store.decode("")},
    ]

    timestamp_signer = signing["TimestampSigner"](key=SECRET_KEY, salt=key_salt)
    max_age_rows = []
    for max_age in (0, -1, 60):
        row = {"max_age": max_age}
        try:
            row["result"] = timestamp_signer.unsign(valid, max_age=max_age)
        except Exception as error:  # noqa: BLE001 - the exception name is the datum
            row["error"] = type(error).__name__
        max_age_rows.append(row)

    expiry_rows = []
    for label, extra in [
        ("default", {}),
        ("zero-seconds", {"_session_expiry": 0}),
        ("seconds", {"_session_expiry": 1200}),
        ("future-iso", {"_session_expiry": "2026-11-27T12:10:00+00:00"}),
        ("past-iso", {"_session_expiry": "2026-11-27T11:00:00+00:00"}),
        ("null", {"_session_expiry": None}),
    ]:
        session = store_class()
        session._session_cache = dict(extra)
        expiry_rows.append(
            {
                "case": label,
                "stored": extra.get("_session_expiry"),
                "expiry_age": session.get_expiry_age(),
                "expiry_date": session.get_expiry_date().isoformat(),
                "expire_at_browser_close": session.get_expire_at_browser_close(),
            }
        )

    set_expiry_rows = []
    for label, value in [
        ("none", None),
        ("zero", 0),
        ("seconds", 1200),
        ("timedelta", datetime.timedelta(minutes=5)),
        ("datetime", CLOCK + datetime.timedelta(seconds=42)),
    ]:
        session = store_class()
        session._session_cache = {"_session_expiry": 1200}
        session.set_expiry(value)
        set_expiry_rows.append(
            {
                "case": label,
                "stored": session._session_cache.get("_session_expiry"),
                "expiry_age": session.get_expiry_age(),
                "expiry_date": session.get_expiry_date().isoformat(),
            }
        )

    validate_rows = [
        {"key": key, "valid": store._validate_session_key(key)}
        for key in (None, "", "a", "abcdefg", "abcdefgh", "k" * 128)
    ]

    recorder.calls.clear()
    plane_instance = store_class()
    plane_instance.exists = lambda session_key: False
    plane_key = plane_instance._get_new_session_key()
    plane_request = recorder.calls[-1]

    recorder.calls.clear()
    django_instance = base_namespace["SessionBase"]()
    django_instance.exists = lambda session_key: False
    django_key = django_instance._get_new_session_key()
    django_request = recorder.calls[-1]

    model_rows = []
    for label, data in [
        ("login-minimal", LOGIN_MINIMAL),
        ("login-device", LOGIN_DEVICE),
        ("device-not-dict", {**LOGIN_MINIMAL, "device_info": "Chrome"}),
        ("no-user", {"device_info": {"device_id": "9c1f2f7a"}}),
        ("empty", {}),
    ]:
        session = store_class(session_key="k" * 128)
        session._session_cache = dict(data)
        obj = session.create_model_instance(data)
        fields = obj.fields
        model_rows.append(
            {
                "case": label,
                "data": data,
                "session_key": fields["session_key"],
                "session_data": fields["session_data"],
                "session_data_matches_encode": fields["session_data"]
                == store.encode(data),
                "expire_date": fields["expire_date"].isoformat(),
                "user_id": fields["user_id"],
                "device_info": fields["device_info"],
            }
        )

    case = {
        "schema": "plane/session-cases@1",
        "python": ".".join(str(part) for part in sys.version_info[:3]),
        "django": django_version(),
        "source": (
            "django/contrib/sessions/backends/base.py (SessionBase, VALID_KEY_CHARS), "
            "django/contrib/sessions/backends/db.py (create_model_instance), "
            "django/core/signing.py (Signer, TimestampSigner, dumps, loads, "
            "JSONSerializer, b62_*, b64_*), "
            "django/utils/crypto.py (salted_hmac, constant_time_compare, "
            "RANDOM_STRING_CHARS), django/conf/global_settings.py "
            "(SESSION_SERIALIZER, SESSION_EXPIRE_AT_BROWSER_CLOSE, USE_TZ), "
            "plane/settings/common.py (SESSION_*, ADMIN_SESSION_*), "
            "plane/db/models/session.py (SessionStore), "
            "plane/db/migrations/0065_auto_20240415_0937.py (Session model)"
        ),
        "clock": {"unix": TIMESTAMP, "iso": CLOCK.isoformat()},
        "secret_key": SECRET_KEY,
        "settings": settings,
        "django_defaults": defaults,
        "signer": {
            "key_salt": key_salt,
            "algorithm": "sha256",
            "separator": ":",
            "base62_alphabet": signing["BASE62_ALPHABET"],
            "compression": "zlib.compress(data), used when shorter than len(data) - 1",
            "serializer": defaults["SESSION_SERIALIZER"],
        },
        "sessions_table": sessions_table(),
        "session_key": {
            "plane": {"length": len(plane_key), "requested": plane_request},
            "django_base": {"length": len(django_key), "requested": django_request},
        },
        "validate_key": validate_rows,
        "encode": encode_rows,
        "decode": decode_rows,
        "max_age": max_age_rows,
        "expiry": expiry_rows,
        "set_expiry": set_expiry_rows,
        "model_instance": model_rows,
    }
    OUT.write_text(
        json.dumps(case, indent=1, ensure_ascii=False) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    print(f"wrote {OUT.relative_to(REPO)}")
    print(
        "encode={} compressed={} decode={} expiry={} set_expiry={} model={}".format(
            len(encode_rows),
            sum(1 for row in encode_rows if row["compressed"]),
            len(decode_rows),
            len(expiry_rows),
            len(set_expiry_rows),
            len(model_rows),
        )
    )


if __name__ == "__main__":
    main()


