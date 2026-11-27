# Copyright (c) 2023-present Plane Software, Inc. and contributors
# SPDX-License-Identifier: AGPL-3.0-only
# See the LICENSE file for details.

"""Record Django's ``pbkdf2_sha256`` decisions without importing Django.

The oracle is the *text* of the Django installed in ``apps/api/.venv``:
``BasePasswordHasher``, ``PBKDF2PasswordHasher``, ``mask_hash``,
``must_update_salt``, and ``is_password_usable`` are pulled out of
``contrib/auth/hashers.py`` with ``ast``; ``pbkdf2`` and
``constant_time_compare`` come from ``utils/crypto.py``; and the hasher order
comes from ``conf/global_settings.py`` (Plane does not override
``PASSWORD_HASHERS``). All of it is executed in a bare namespace, so the answers
come from Django's own code and not from a re-typed copy.

Only ``force_bytes`` is re-typed: Django's ``pbkdf2`` calls it, and every input
here is ``str``, so it is ``s.encode("utf-8")``. ``is_protected_type`` is never
reached because ``strings_only`` stays ``False``.

No Django import, no database, no container. Run from the repo root with the
interpreter the API targets (``apps/api/pyproject.toml`` says
``>=3.12,<3.13``):

    apps\\api\\.venv\\Scripts\\python.exe contracts\\tools\\capture_password.py

The output is ``contracts/golden/password-cases.json``; re-running it must not
change a byte. ``cargo test -p plane-pkg --test password_golden`` replays it.
"""

import ast
import base64
import hashlib
import hmac as hmac_module
import json
import math
import pathlib
import secrets
import sys
import warnings

REPO = pathlib.Path(__file__).resolve().parents[2]
VENV = REPO / "apps" / "api" / ".venv"
OUT = REPO / "contracts" / "golden" / "password-cases.json"
SITE_DIRS = ("Lib/site-packages", "lib/python3.12/site-packages")


def site_packages():
    for relative in SITE_DIRS:
        candidate = VENV / relative
        if (candidate / "django").is_dir():
            return candidate
    raise SystemExit(f"django is not installed under {VENV}")


SITE = site_packages()
HASHERS = SITE / "django" / "contrib" / "auth" / "hashers.py"
CRYPTO = SITE / "django" / "utils" / "crypto.py"
GLOBALS = SITE / "django" / "conf" / "global_settings.py"


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


def module_constant(path, name):
    """Read a module-level ``NAME = <literal>`` without executing the module."""
    tree, _ = module_level(path, ())
    for node in tree.body:
        if not isinstance(node, ast.Assign):
            continue
        for target in node.targets:
            if isinstance(target, ast.Name) and target.id == name:
                return ast.literal_eval(node.value)
    raise SystemExit(f"{path.name}: {name} not found")


def exec_nodes(nodes, namespace, origin):
    module = ast.Module(body=list(nodes), type_ignores=[])
    exec(compile(module, str(origin), "exec"), namespace)


def build():
    random_chars = module_constant(CRYPTO, "RANDOM_STRING_CHARS")

    def force_bytes(value):
        if not isinstance(value, str):
            raise AssertionError("capture only passes str to pbkdf2")
        return value.encode("utf-8")

    crypto = {
        "hashlib": hashlib,
        "secrets": secrets,
        "hmac": hmac_module,
        "force_bytes": force_bytes,
    }
    _, crypto_nodes = module_level(CRYPTO, ("pbkdf2", "constant_time_compare"))
    exec_nodes(crypto_nodes, crypto, CRYPTO)

    namespace = {
        "base64": base64,
        "hashlib": hashlib,
        "math": math,
        "warnings": warnings,
        "RANDOM_STRING_CHARS": random_chars,
        "UNUSABLE_PASSWORD_PREFIX": module_constant(HASHERS, "UNUSABLE_PASSWORD_PREFIX"),
        "pbkdf2": crypto["pbkdf2"],
        "constant_time_compare": crypto["constant_time_compare"],
        "get_random_string": lambda length, allowed_chars=random_chars: "".join(
            secrets.choice(allowed_chars) for _ in range(length)
        ),
        "NotImplementedError": NotImplementedError,
    }
    _, hasher_nodes = module_level(
        HASHERS,
        (
            "BasePasswordHasher",
            "PBKDF2PasswordHasher",
            "mask_hash",
            "must_update_salt",
            "is_password_usable",
        ),
    )
    exec_nodes(hasher_nodes, namespace, HASHERS)
    namespace["_"] = lambda message: message
    return namespace, random_chars


def main():
    namespace, random_chars = build()
    hasher_class = namespace["PBKDF2PasswordHasher"]
    hasher = hasher_class()
    mask_hash = namespace["mask_hash"]
    must_update_salt = namespace["must_update_salt"]
    usable = namespace["is_password_usable"]
    compare = namespace["constant_time_compare"]

    password = "correct horse battery staple"
    salt = "vB6t9Xq2LmRw4ZpYc8Hd3g"
    short_salt = "vB6t9Xq2LmRw4ZpYc8Hd3"
    old_iterations = 260_000
    fast_iterations = 1_000

    def encode(secret, salt_value, iterations=None):
        return hasher.encode(secret, salt_value, iterations)

    def policy(iterations):
        """A hasher with a different work factor, same code path."""
        other = hasher_class()
        other.iterations = iterations
        return other

    def recorded(action, slow=False, **fields):
        row = dict(fields)
        if slow:
            row["slow"] = True
        try:
            row.update(action())
        except Exception as error:  # noqa: BLE001 - the exception name is the datum
            row["error"] = type(error).__name__
        return row

    fast = encode(password, salt, fast_iterations)
    production = encode(password, salt)
    old = encode(password, salt, old_iterations)

    encode_cases = [
        recorded(
            lambda secret=secret, salt_value=salt_value, iterations=iterations: {
                "encoded": encode(secret, salt_value, iterations)
            },
            slow=iterations is None,
            password=secret,
            salt=salt_value,
            iterations=iterations,
        )
        for secret, salt_value, iterations in [
            (password, salt, None),
            (password, salt, fast_iterations),
            (password, salt, 1),
            ("pässwörd", salt, fast_iterations),
            ("", salt, fast_iterations),
            (password, "", fast_iterations),
        ]
    ]

    wrong = "wrong password"
    verify_encoded_sha1 = f"pbkdf2_sha1${fast_iterations}${salt}$abc"
    verify_cases = [
        recorded(lambda: {"ok": hasher.verify(password, fast)}, password=password, encoded=fast),
        recorded(lambda: {"ok": hasher.verify(wrong, fast)}, password=wrong, encoded=fast),
        recorded(
            lambda: {"ok": hasher.verify(password, old)},
            slow=True,
            password=password,
            encoded=old,
        ),
        recorded(
            lambda: {"ok": hasher.verify(wrong, production)},
            slow=True,
            password=wrong,
            encoded=production,
        ),
        recorded(
            lambda: {"ok": hasher.verify(password, verify_encoded_sha1)},
            password=password,
            encoded=verify_encoded_sha1,
        ),
        recorded(lambda: {"ok": hasher.verify(password, "garbage")}, password=password, encoded="garbage"),
        recorded(lambda: {"ok": hasher.verify(password, "!")}, password=password, encoded="!"),
    ]


    parse_cases = []
    for text in ["1_000", " 1000 ", "+1000", "", "1__0", "_1", "1_", "1.5", "abc", "-1"]:
        encoded = f"pbkdf2_sha256${text}$salt$hash"
        row = {"iterations_text": text, "encoded": encoded}
        try:
            row["iterations"] = hasher.decode(encoded)["iterations"]
        except Exception as error:  # noqa: BLE001 - the exception name is the datum
            row["error"] = type(error).__name__
        parse_cases.append(row)

    must_update_cases = []
    for encoded, current, note in [
        (fast, fast_iterations, "same work factor"),
        (encode(password, salt, 999), fast_iterations, "one iteration short"),
        (encode(password, short_salt, fast_iterations), fast_iterations, "21-character salt"),
        (production, 1_000_000, "production work factor"),
    ]:
        must_update_cases.append(
            {
                "encoded": encoded,
                "policy_iterations": current,
                "note": note,
                "must_update": policy(current).must_update(encoded),
            }
        )

    salt_cases = [
        {
            "salt": "k" * length,
            "must_update": must_update_salt("k" * length, hasher.salt_entropy),
        }
        for length in (1, 20, 21, 22, 23)
    ]

    spent = []
    original_pbkdf2 = namespace["pbkdf2"]

    def recording_pbkdf2(secret, salt_value, iterations, digest=None):
        spent.append(iterations)
        return original_pbkdf2(secret, salt_value, iterations, digest=digest)

    namespace["pbkdf2"] = recording_pbkdf2
    harden_cases = []
    for encoded, current in [
        (encode(password, salt, 900), 1_000),
        (encode(password, salt, 1_000), 1_000),
        (encode(password, salt, 1_000), 500),
    ]:
        spent.clear()
        policy(current).harden_runtime(password, encoded)
        harden_cases.append(
            {
                "encoded": encoded,
                "policy_iterations": current,
                "spent_iterations": sum(spent),
            }
        )

    mask_cases = [
        {"hash": value, "show": show, "masked": mask_hash(value, show)}
        for value, show in [
            ("abcdefghij", 6),
            ("short", 6),
            (verify_cases[0]["encoded"].split("$", 3)[3], 6),
        ]
    ]

    compare_cases = [
        {"left": left, "right": right, "equal": compare(left, right)}
        for left, right in [
            ("abc", "abc"),
            ("abc", "abd"),
            ("abc", "abcd"),
            ("pässwörd", "pässwörd"),
        ]
    ]

    case = {
        "schema": "plane/password-cases@1",
        "python": ".".join(str(part) for part in sys.version_info[:3]),
        "django": django_version(),
        "source": (
            "django/contrib/auth/hashers.py (BasePasswordHasher, PBKDF2PasswordHasher, "
            "mask_hash, must_update_salt, is_password_usable), "
            "django/utils/crypto.py (pbkdf2, constant_time_compare, RANDOM_STRING_CHARS), "
            "django/conf/global_settings.py (PASSWORD_HASHERS)"
        ),
        "password_hashers": module_constant(GLOBALS, "PASSWORD_HASHERS"),
        "hasher": {
            "algorithm": hasher.algorithm,
            "iterations": hasher.iterations,
            "digest": hasher.digest().name,
            "salt_entropy": hasher.salt_entropy,
            "salt_length": len(hasher.salt()),
            "random_string_chars": random_chars,
        },
        "encode": encode_cases,
        "verify": verify_cases,
        "parse": parse_cases,
        "must_update": must_update_cases,
        "must_update_salt": salt_cases,
        "harden_runtime": harden_cases,
        "mask_hash": mask_cases,
        "constant_time_compare": compare_cases,
        "usable": [
            {"encoded": value, "usable": usable(value)}
            for value in [
                None,
                "pbkdf2_sha256$1$salt$hash",
                "!0123456789012345678901234567890123456789",
            ]
        ],
    }
    OUT.write_text(
        json.dumps(case, indent=1, ensure_ascii=False) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    print(f"wrote {OUT.relative_to(REPO)}")


if __name__ == "__main__":
    main()

