# Copyright (c) 2023-present Plane Software, Inc. and contributors
# SPDX-License-Identifier: AGPL-3.0-only
# See the LICENSE file for details.

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "route_parity", ROOT / "contracts" / "tools" / "route_parity.py"
)
route_parity = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(route_parity)


def payload(*routes):
    return {"routes": list(routes)}


def route(
    path,
    owner="django",
    status="red",
    order=1,
    methods=("GET",),
    reason=None,
):
    value = {
        "path": path,
        "methods": list(methods),
        "owner": owner,
        "status": status,
        "order": order,
    }
    if reason is not None:
        value["reason"] = reason
    return value


def implemented_route(path, implemented=True, order=1, methods=("GET",)):
    return {
        "path": path,
        "methods": list(methods),
        "order": order,
        "implemented": implemented,
    }


class OwnerGateTest(unittest.TestCase):
    def compare(self, django_routes, rust_routes=()):
        return route_parity.compare(
            route_parity._django_routes(payload(*django_routes)),
            route_parity._rust_routes(payload(*rust_routes)),
        )

    def test_valid_owner_is_green(self):
        report = self.compare(
            [route("api/x/", owner="rust", status="green")],
            [implemented_route("api/x/")],
        )
        self.assertEqual(report["owner_missing"], [])
        self.assertEqual(report["owner_invalid"], [])
        self.assertEqual(report["owner_counts"], {"rust": 1})
        self.assertEqual(report["status_counts"], {"green": 1})
        self.assertTrue(route_parity.is_green(report))

    def test_missing_owner_is_not_green(self):
        report = self.compare([route("api/x/", owner=None)])
        self.assertEqual(len(report["owner_missing"]), 1)
        self.assertFalse(route_parity.is_green(report))

    def test_invalid_owner_is_not_green(self):
        report = self.compare([route("api/x/", owner="both")])
        self.assertEqual(len(report["owner_invalid"]), 1)
        self.assertFalse(route_parity.is_green(report))

    def test_owner_is_not_hidden_by_allow_missing(self):
        report = self.compare([route("api/x/", owner=None)])
        missing = [{"method": "GET", "path": "api/x/"}]
        self.assertFalse(route_parity.is_green(dict(report, missing=missing)))

    def test_rust_owner_must_be_implemented(self):
        report = self.compare(
            [route("api/x/", owner="rust", status="green")],
            [implemented_route("api/x/", implemented=False)],
        )
        self.assertEqual(len(report["owner_impl_missing"]), 1)
        self.assertFalse(route_parity.is_green(report))
        self.assertFalse(route_parity.owner_green(report))

    def test_django_owner_cannot_be_served_by_rust(self):
        report = self.compare(
            [route("api/x/", owner="django", status="red")],
            [implemented_route("api/x/")],
        )
        self.assertEqual(len(report["owner_impl_conflict"]), 1)
        self.assertFalse(route_parity.is_green(report))
        self.assertFalse(route_parity.owner_green(report))

    def test_green_status_requires_a_rust_implementation(self):
        report = self.compare([route("api/x/", owner="django", status="green")])
        self.assertEqual(len(report["status_false_green"]), 1)
        self.assertFalse(route_parity.is_green(report))
        self.assertFalse(route_parity.owner_green(report))

    def test_waiver_requires_a_reason(self):
        report = self.compare(
            [route("api/x/", owner="django", status="waived")]
        )
        self.assertEqual(len(report["status_waiver_missing_reason"]), 1)
        self.assertFalse(route_parity.is_green(report))

    def test_missing_or_invalid_status_is_not_green(self):
        missing = self.compare([route("api/x/", status=None)])
        invalid = self.compare([route("api/x/", status="blue")])
        self.assertEqual(len(missing["status_missing"]), 1)
        self.assertEqual(len(invalid["status_invalid"]), 1)
        self.assertFalse(route_parity.is_green(missing))
        self.assertFalse(route_parity.is_green(invalid))


class RepositoryContractTest(unittest.TestCase):
    def test_checked_in_artifacts_agree(self):
        django = route_parity._django_routes(
            json.loads(
                (ROOT / "contracts" / "routes.json").read_text(encoding="utf-8")
            )
        )
        rust = route_parity._rust_routes(
            json.loads(
                (ROOT / "apps" / "api-rs" / "contracts" / "routes.rust.json")
                .read_text(encoding="utf-8")
            )
        )
        report = route_parity.compare(django, rust)

        # Rust serves only the web door today. The ownership/status gate must
        # stay green while full route parity is still transitioning.
        self.assertTrue(route_parity.owner_green(report))
        self.assertEqual(report["owner_counts"], {"django": 720, "rust": 2})
        self.assertEqual(report["status_counts"], {"green": 2, "red": 720})


class DumpRoutesTest(unittest.TestCase):
    def test_contract_state_is_merged_by_method(self):
        spec = importlib.util.spec_from_file_location(
            "dump_routes",
            ROOT / "contracts" / "tools" / "dump_routes.py",
        )
        dump_routes = importlib.util.module_from_spec(spec)
        # The module imports Django, which is available inside the API image but
        # not necessarily in a host Python. Test the pure merge helper only.
        source = (
            ROOT / "contracts" / "tools" / "dump_routes.py"
        ).read_text(encoding="utf-8")
        namespace = {"Path": Path, "json": json, "STATUSES": {"green", "red", "waived"}}
        for name in ("_load_json", "_owner_key", "_owner_for", "_status_for",
                     "apply_contract_state"):
            start = source.index(f"def {name}(")
            end = source.index("\n\ndef ", start)
            exec(compile(source[start:end], "dump_routes.py", "exec"), namespace)

        routes = [
            {
                "path": "api/x/",
                "methods": ["GET", "POST"],
                "owner": "django",
                "statuses": {},
            }
        ]
        owners = {"GET api/x/": "rust", "POST api/x/": "rust"}
        statuses = {
            "GET api/x/": {"status": "green"},
            "POST api/x/": {"status": "waived", "reason": "ticket-1"},
        }
        namespace["apply_contract_state"](routes, owners, statuses)
        self.assertEqual(routes[0]["owner"], "rust")
        self.assertEqual(routes[0]["statuses"]["GET"]["status"], "green")
        self.assertEqual(routes[0]["statuses"]["POST"]["reason"], "ticket-1")


class CheckedInEndpointTest(unittest.TestCase):
    def test_routes_schema_version_matches_generator(self):
        payload = json.loads(
            (ROOT / "contracts" / "routes.json").read_text(encoding="utf-8")
        )
        source = (ROOT / "contracts" / "tools" / "dump_routes.py").read_text(
            encoding="utf-8"
        )
        # The snapshot is generated; a generator that writes a different schema
        # than the checked-in file silently downgrades the next refresh.
        self.assertEqual(payload["schema_version"], 3)
        self.assertIn('"schema_version": 3,', source)

    def test_endpoints_csv_agrees_with_contract_files(self):
        import csv

        owners = json.loads(
            (ROOT / "contracts" / "route-owners.json").read_text(encoding="utf-8")
        )["owners"]
        statuses = json.loads(
            (ROOT / "contracts" / "endpoint-status.json").read_text(encoding="utf-8")
        )["statuses"]
        rust = {
            (method, route["path"])
            for route in json.loads(
                (
                    ROOT / "apps" / "api-rs" / "contracts" / "routes.rust.json"
                ).read_text(encoding="utf-8")
            )["routes"]
            for method in route["methods"]
        }
        rows = list(
            csv.DictReader(
                (ROOT / "contracts" / "endpoints.csv").open(
                    encoding="utf-8", newline=""
                )
            )
        )
        self.assertEqual(len(rows), 722)
        self.assertEqual(
            sorted(row["status"] for row in rows).count("green"), 2
        )
        for row in rows:
            key = f"{row['method']} {row['path']}"
            owner = owners.get(key, owners.get(row["path"]))
            status = statuses.get(key, statuses.get(row["path"]))
            declared_owner = owner["owner"] if isinstance(owner, dict) else owner
            declared_status = (
                status["status"] if isinstance(status, dict) else status
            )
            self.assertEqual(row["owner"], declared_owner, key)
            self.assertEqual(row["status"], declared_status, key)
            if row["status"] == "green":
                self.assertEqual(row["owner"], "rust", key)
                self.assertIn((row["method"], row["path"]), rust, key)
                self.assertTrue(row["reason"], key)

    def test_root_status_reason_names_the_root_case(self):
        statuses = json.loads(
            (ROOT / "contracts" / "endpoint-status.json").read_text(encoding="utf-8")
        )["statuses"]
        self.assertIn("django-root", statuses["* "]["reason"])
        self.assertNotIn("django-robots", statuses["* "]["reason"])

    def test_green_wire_reason_matches_endpoint_status_for_root_and_robots(self):
        import csv

        statuses = json.loads(
            (ROOT / "contracts" / "endpoint-status.json").read_text(encoding="utf-8")
        )["statuses"]
        rows = {
            f"{row['method']} {row['path']}": row
            for row in csv.DictReader(
                (ROOT / "contracts" / "endpoints.csv").open(
                    encoding="utf-8", newline=""
                )
            )
        }
        for key in ("* ", "* robots.txt"):
            self.assertEqual(rows[key]["reason"], statuses[key]["reason"], key)


if __name__ == "__main__":
    unittest.main()
