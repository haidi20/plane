#!/usr/bin/env bash
# Copyright (c) 2023-present Plane Software, Inc. and contributors
# SPDX-License-Identifier: AGPL-3.0-only
# See the LICENSE file for details.

# Seed minimal real data (workspace -> project -> issues) through the public
# HTTP API only. Idempotent: existing entities are reused by slug/identifier.
set -euo pipefail

BASE_URL="${BASE_URL:-http://localhost:8080}"
COOKIE_JAR="${COOKIE_JAR:-/tmp/plane-cookies.txt}"
UA="${UA:-Mozilla/5.0 (compatible; planesync/1.0)}"
WS_NAME="${WS_NAME:-Rust Parity}"
WS_SLUG="${WS_SLUG:-rust-parity}"
PROJECT_NAME="${PROJECT_NAME:-MVP Parity}"
PROJECT_IDENTIFIER="${PROJECT_IDENTIFIER:-MVPP}"
OUT="${OUT:-/tmp/plane-fixtures.json}"

if [[ ! -f "$COOKIE_JAR" ]]; then
  echo "missing cookie jar $COOKIE_JAR; run bootstrap-local.sh first" >&2
  exit 1
fi

csrf() {
  awk '$6 == "csrftoken" { print $7 }' "$COOKIE_JAR" | tail -1
}

api() {
  local method="$1" path="$2" body="${3:-}"
  if [[ -n "$body" ]]; then
    curl -sS -A "$UA" -b "$COOKIE_JAR" -c "$COOKIE_JAR" \
      -H "X-CSRFToken: $(csrf)" -H "Content-Type: application/json" \
      -H "Origin: $BASE_URL" -H "Referer: $BASE_URL/" \
      -X "$method" -d "$body" -w '\n%{http_code}' "$BASE_URL$path"
  else
    curl -sS -A "$UA" -b "$COOKIE_JAR" -c "$COOKIE_JAR" \
      -H "X-CSRFToken: $(csrf)" -H "Origin: $BASE_URL" -H "Referer: $BASE_URL/" \
      -X "$method" -w '\n%{http_code}' "$BASE_URL$path"
  fi
}

split_response() {
  local raw="$1"
  HTTP_CODE="${raw##*$'\n'}"
  HTTP_BODY="${raw%$'\n'*}"
}

# --- workspace ---------------------------------------------------------------
# NOTE: GET /api/workspaces/ returns 400 in Django (KeyError 'slug' from the
# workspace permission decorator). The app lists workspaces through
# GET /api/users/me/workspaces/ instead; that behaviour must be preserved.
split_response "$(api GET /api/users/me/workspaces/)"
[[ "$HTTP_CODE" == "200" ]] || { echo "list workspaces failed: $HTTP_CODE $HTTP_BODY" >&2; exit 1; }
WS_ID="$(printf '%s' "$HTTP_BODY" | python3 -c '
import json, sys
slug = sys.argv[1]
data = json.load(sys.stdin)
rows = data if isinstance(data, list) else data.get("results", [])
match = [w for w in rows if w.get("slug") == slug]
print(match[0]["id"] if match else "")
' "$WS_SLUG")"

if [[ -z "$WS_ID" ]]; then
  split_response "$(api POST /api/workspaces/ "$(python3 -c '
import json, sys
print(json.dumps({"name": sys.argv[1], "slug": sys.argv[2]}))
' "$WS_NAME" "$WS_SLUG")")"
  [[ "$HTTP_CODE" == "201" ]] || { echo "create workspace failed: $HTTP_CODE $HTTP_BODY" >&2; exit 1; }
  WS_ID="$(printf '%s' "$HTTP_BODY" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
  echo "created workspace $WS_SLUG ($WS_ID)"
else
  echo "reusing workspace $WS_SLUG ($WS_ID)"
fi

# --- project -----------------------------------------------------------------
split_response "$(api GET "/api/workspaces/$WS_SLUG/projects/")"
[[ "$HTTP_CODE" == "200" ]] || { echo "list projects failed: $HTTP_CODE $HTTP_BODY" >&2; exit 1; }
PROJECT_ID="$(printf '%s' "$HTTP_BODY" | python3 -c '
import json, sys
identifier = sys.argv[1]
data = json.load(sys.stdin)
rows = data if isinstance(data, list) else data.get("results", [])
match = [p for p in rows if p.get("identifier") == identifier]
print(match[0]["id"] if match else "")
' "$PROJECT_IDENTIFIER")"

if [[ -z "$PROJECT_ID" ]]; then
  split_response "$(api POST "/api/workspaces/$WS_SLUG/projects/" "$(python3 -c '
import json, sys
print(json.dumps({"name": sys.argv[1], "identifier": sys.argv[2]}))
' "$PROJECT_NAME" "$PROJECT_IDENTIFIER")")"
  [[ "$HTTP_CODE" == "201" ]] || { echo "create project failed: $HTTP_CODE $HTTP_BODY" >&2; exit 1; }
  PROJECT_ID="$(printf '%s' "$HTTP_BODY" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
  echo "created project $PROJECT_IDENTIFIER ($PROJECT_ID)"
else
  echo "reusing project $PROJECT_IDENTIFIER ($PROJECT_ID)"
fi

# --- issues ------------------------------------------------------------------
EXISTING_ISSUES="$(api GET "/api/workspaces/$WS_SLUG/projects/$PROJECT_ID/issues/" | sed '$d')"
for name in "Wire parity baseline" "Pagination and filters" "Attachments and uploads"; do
  exists="$(printf '%s' "$EXISTING_ISSUES" | python3 -c '
import json, sys
name = sys.argv[1]
data = json.load(sys.stdin)
rows = data if isinstance(data, list) else data.get("results", [])
print("1" if any(i.get("name") == name for i in rows) else "")
' "$name")"
  if [[ -n "$exists" ]]; then
    echo "reusing issue '$name'"
    continue
  fi
  split_response "$(api POST "/api/workspaces/$WS_SLUG/projects/$PROJECT_ID/issues/" "$(python3 -c '
import json, sys
print(json.dumps({"name": sys.argv[1]}))
' "$name")")"
  if [[ "$HTTP_CODE" != "201" ]]; then
    echo "create issue '$name' failed: $HTTP_CODE $HTTP_BODY" >&2
    exit 1
  fi
  id="$(printf '%s' "$HTTP_BODY" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
  echo "created issue '$name' ($id)"
done

python3 - "$WS_ID" "$WS_SLUG" "$PROJECT_ID" "$PROJECT_IDENTIFIER" "$OUT" <<'PY'
import json, sys
ws_id, ws_slug, project_id, project_identifier, out = sys.argv[1:6]
payload = {
    "workspace_id": ws_id,
    "workspace_slug": ws_slug,
    "project_id": project_id,
    "project_identifier": project_identifier,
}
open(out, "w").write(json.dumps(payload, indent=2) + "\n")
print("fixtures ->", out)
PY