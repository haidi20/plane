#!/usr/bin/env bash
# Copyright (c) 2023-present Plane Software, Inc. and contributors
# SPDX-License-Identifier: AGPL-3.0-only
# See the LICENSE file for details.

# Capture golden request/response pairs from the Django process itself.
#
# Everything here talks HTTP to 127.0.0.1:8000 inside the api container, so the
# capture does not depend on Caddy, the web container, or a pre-seeded cookie
# jar. The community proxy routes "/" and "/robots.txt" to the web container,
# so Django's own answer for those two paths can only be observed this way.
#
# Requires a running api container and the admin account from bootstrap-local.sh.
set -euo pipefail

CONTAINER="${CONTAINER:-community-api-1}"
OUT_DIR="${OUT_DIR:-/mnt/d/projects/plane/contracts/golden}"
INTERNAL_URL="${INTERNAL_URL:-http://127.0.0.1:8000}"
EMAIL="${EMAIL:-admin@plane.local}"
PASSWORD="${PASSWORD:-PlaneAdmin!2345}"
WS_SLUG="${WS_SLUG:-rust-parity}"
PROJECT_IDENTIFIER="${PROJECT_IDENTIFIER:-MVPP}"
STAGE=/tmp/golden-internal

docker exec "$CONTAINER" rm -rf "$STAGE"
docker exec -i \
  -e INTERNAL_URL="$INTERNAL_URL" \
  -e EMAIL="$EMAIL" \
  -e PASSWORD="$PASSWORD" \
  -e WS_SLUG="$WS_SLUG" \
  -e PROJECT_IDENTIFIER="$PROJECT_IDENTIFIER" \
  "$CONTAINER" python3 - "$STAGE" <<'PY'
import http.cookiejar
import json
import os
import sys
import tarfile
import urllib.error
import urllib.parse
import urllib.request

stage = sys.argv[1]
base = os.environ["INTERNAL_URL"].rstrip("/")
email = os.environ["EMAIL"]
password = os.environ["PASSWORD"]
ws_slug = os.environ["WS_SLUG"]
project_identifier = os.environ["PROJECT_IDENTIFIER"]

class NoRedirect(urllib.request.HTTPRedirectHandler):
    """Keep Django's 302/Location as data instead of following it.

    Sign-in redirects to APP_DOMAIN (localhost:8080), which is not reachable
    from inside the api container. The redirect is part of the contract, so we
    record it rather than chase it.
    """

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


jar = http.cookiejar.CookieJar()
opener = urllib.request.build_opener(
    urllib.request.HTTPCookieProcessor(jar), NoRedirect()
)
UA = "Mozilla/5.0 (compatible; planesync/1.0)"


def call(method, path, body=None, form=None, headers=None):
    url = base + path
    data = None
    request_headers = {"User-Agent": UA, "Accept": "application/json"}
    if headers:
        request_headers.update(headers)
    if form is not None:
        data = urllib.parse.urlencode(form).encode()
        request_headers["Content-Type"] = "application/x-www-form-urlencoded"
    elif body is not None:
        data = json.dumps(body).encode()
        request_headers["Content-Type"] = "application/json"
    request = urllib.request.Request(url, data=data, headers=request_headers, method=method)
    try:
        response = opener.open(request, timeout=20)
        return response.status, response.headers, response.read()
    except urllib.error.HTTPError as exc:
        return exc.code, exc.headers, exc.read()


def csrf():
    status, _headers, payload = call("GET", "/auth/get-csrf-token/")
    if status != 200:
        raise SystemExit(f"csrf token failed: HTTP {status}")
    return json.loads(payload)["csrf_token"]


def form_post(path, fields):
    token = csrf()
    return call("POST", path, form=fields, headers={
        "X-CSRFToken": token,
        "Origin": "http://localhost:8080",
        "Referer": "http://localhost:8080/",
    })


status, _headers, _payload = form_post(
    "/auth/sign-in/", {"email": email, "password": password}
)
if status not in (302, 303):
    raise SystemExit(f"sign-in failed: HTTP {status}")


def api(method, path, body=None):
    return call(method, path, body=body, headers={
        "Origin": "http://localhost:8080",
        "Referer": "http://localhost:8080/",
    })


def json_body(status, payload, what):
    if status >= 400:
        raise SystemExit(f"{what} failed: HTTP {status} {payload[:300]!r}")
    return json.loads(payload)


status, _headers, payload = api("GET", "/api/users/me/workspaces/")
rows = json_body(status, payload, "list workspaces")
rows = rows if isinstance(rows, list) else rows.get("results", [])
match = [w for w in rows if w.get("slug") == ws_slug]
if not match:
    status, _headers, payload = api("POST", "/api/workspaces/", {"name": "Rust Parity", "slug": ws_slug})
    workspace = json_body(status, payload, "create workspace")
else:
    workspace = match[0]

status, _headers, payload = api("GET", f"/api/workspaces/{ws_slug}/projects/")
rows = json_body(status, payload, "list projects")
rows = rows if isinstance(rows, list) else rows.get("results", [])
match = [p for p in rows if p.get("identifier") == project_identifier]
if not match:
    status, _headers, payload = api(
        "POST",
        f"/api/workspaces/{ws_slug}/projects/",
        {"name": "MVP Parity", "identifier": project_identifier},
    )
    project = json_body(status, payload, "create project")
else:
    project = match[0]

project_id = project["id"]

status, _headers, payload = api(
    "GET", f"/api/workspaces/{ws_slug}/projects/{project_id}/issues/"
)
rows = json_body(status, payload, "list issues")
rows = rows if isinstance(rows, list) else rows.get("results", [])
for name in ("Wire parity baseline", "Pagination and filters", "Attachments and uploads"):
    if not any(issue.get("name") == name for issue in rows):
        api(
            "POST",
            f"/api/workspaces/{ws_slug}/projects/{project_id}/issues/",
            {"name": name},
        )
status, _headers, payload = api(
    "GET", f"/api/workspaces/{ws_slug}/projects/{project_id}/issues/"
)
rows = json_body(status, payload, "list issues after seed")
rows = rows if isinstance(rows, list) else rows.get("results", [])
if not rows:
    raise SystemExit("no issues available for golden capture")
issue_id = rows[0]["id"]


def header_subset(headers):
    kept = {key.lower(): value for key, value in headers.items()}
    return [
        (key.title(), kept[key])
        for key in ("content-type", "allow", "location", "x-robots-tag")
        if key in kept
    ]


def write_case(name, method, path, status, headers, body):
    directory = os.path.join(stage, name)
    os.makedirs(directory, exist_ok=True)

    with open(os.path.join(directory, "request.http"), "w", newline="\n") as handle:
        handle.write(f"{method} {path} HTTP/1.1\n")
        handle.write("Host: 127.0.0.1:8000\n")
        handle.write(f"User-Agent: {UA}\n")
        handle.write("Accept: application/json\n")
        handle.write("Cookie: <session cookie from live sign-in>\n\n")

    with open(os.path.join(directory, "response.http"), "w", newline="\n") as handle:
        handle.write(f"HTTP/1.1 {status}\n")
        for key, value in sorted(header_subset(headers)):
            handle.write(f"{key}: {value}\n")
        handle.write("\n")

    try:
        parsed = json.loads(body)
    except ValueError:
        with open(os.path.join(directory, "body.raw"), "wb") as handle:
            handle.write(body)
        kind = "raw"
    else:
        with open(os.path.join(directory, "body.json"), "w", newline="\n") as handle:
            json.dump(parsed, handle, indent=2, sort_keys=True)
            handle.write("\n")
        kind = "json"

    print(f"{name}: HTTP/1.1 {status} ({kind})")


def capture(name, method, path):
    status, headers, body = api(method, path)
    write_case(name, method, path, status, headers, body)


cases = [
    ("django-root", "GET", "/"),
    ("django-robots", "GET", "/robots.txt"),
    ("web-root", "GET", "/"),
    ("web-robots", "GET", "/robots.txt"),
    ("users-me", "GET", "/api/users/me/"),
    ("users-me-workspaces", "GET", "/api/users/me/workspaces/"),
    ("workspaces-root", "GET", "/api/workspaces/"),
    ("workspace-detail", "GET", f"/api/workspaces/{ws_slug}/"),
    ("project-list", "GET", f"/api/workspaces/{ws_slug}/projects/"),
    ("project-detail", "GET", f"/api/workspaces/{ws_slug}/projects/{project_id}/"),
    ("issue-list", "GET", f"/api/workspaces/{ws_slug}/projects/{project_id}/issues/"),
    ("issue-detail", "GET", f"/api/workspaces/{ws_slug}/projects/{project_id}/issues/{issue_id}/"),
    ("state-list", "GET", f"/api/workspaces/{ws_slug}/projects/{project_id}/states/"),
    ("label-list", "GET", f"/api/workspaces/{ws_slug}/projects/{project_id}/issue-labels/"),
    ("member-list", "GET", f"/api/workspaces/{ws_slug}/members/"),
    ("cycle-list", "GET", f"/api/workspaces/{ws_slug}/projects/{project_id}/cycles/"),
]

# Both "/" and "/robots.txt" answer from Django here. The web-root/web-robots
# cases keep the names used by the public (Caddy) capture so the two can be
# diffed case by case.
for name, method, path in cases:
    capture(name, method, path)

with tarfile.open("/tmp/golden-internal.tar", "w") as tar:
    tar.add(stage, arcname="golden-internal")
print("staged")
PY

docker cp "$CONTAINER:/tmp/golden-internal.tar" /tmp/golden-internal.tar
rm -rf /tmp/golden-internal
tar -xf /tmp/golden-internal.tar -C /tmp
for directory in /tmp/golden-internal/*/; do
  name="$(basename "$directory")"
  mkdir -p "$OUT_DIR/$name"
  rm -f "$OUT_DIR/$name/body.raw" "$OUT_DIR/$name/body.json"
  cp -f "$directory"request.http "$OUT_DIR/$name/request.http"
  cp -f "$directory"response.http "$OUT_DIR/$name/response.http"
  if [[ -f "$directory/body.json" ]]; then
    cp -f "$directory/body.json" "$OUT_DIR/$name/body.json"
  fi
  if [[ -f "$directory/body.raw" ]]; then
    cp -f "$directory/body.raw" "$OUT_DIR/$name/body.raw"
  fi
  echo "$name: $(head -1 "$OUT_DIR/$name/response.http")"
done
echo "internal capture complete -> $OUT_DIR"
