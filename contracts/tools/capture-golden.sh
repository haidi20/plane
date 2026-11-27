#!/usr/bin/env bash
# Copyright (c) 2023-present Plane Software, Inc. and contributors
# SPDX-License-Identifier: AGPL-3.0-only
# See the LICENSE file for details.

# Capture golden request/response pairs from the real Django API.
# Every artifact is produced by real HTTP against real seeded data; nothing is
# mocked and nothing is generated from the ORM.
set -euo pipefail

BASE_URL="${BASE_URL:-http://localhost:8080}"
COOKIE_JAR="${COOKIE_JAR:-/tmp/plane-cookies.txt}"
UA="${UA:-Mozilla/5.0 (compatible; planesync/1.0)}"
FIXTURES="${FIXTURES:-/tmp/plane-fixtures.json}"
GOLDEN_DIR="${GOLDEN_DIR:-/mnt/d/projects/plane/contracts/golden}"

eval "$(python3 - "$FIXTURES" <<'PY'
import json, shlex, sys
data = json.load(open(sys.argv[1]))
for key in ("workspace_id", "workspace_slug", "project_id", "project_identifier"):
    print(f"{key.upper()}={shlex.quote(str(data[key]))}")
PY
)"

ISSUE_ID="$(curl -sS -A "$UA" -b "$COOKIE_JAR" \
  -H "Referer: $BASE_URL/" \
  "$BASE_URL/api/workspaces/$WORKSPACE_SLUG/projects/$PROJECT_ID/issues/" |
  python3 -c 'import json,sys; d=json.load(sys.stdin); rows=d if isinstance(d,list) else d.get("results",[]); print(rows[0]["id"] if rows else "")')"

[[ -n "$ISSUE_ID" ]] || { echo "no issue found for golden capture" >&2; exit 1; }

capture() {
  local name="$1" method="$2" path="$3" body="${4:-}"
  local dir="$GOLDEN_DIR/$name"
  mkdir -p "$dir"

  {
    printf '%s %s HTTP/1.1\n' "$method" "$path"
    printf 'Host: %s\n' "${BASE_URL#http://}"
    printf 'User-Agent: %s\n' "$UA"
    printf 'Accept: application/json\n'
    [[ -n "$body" ]] && { printf 'Content-Type: application/json\n'; printf 'Content-Length: %s\n' "$(printf '%s' "$body" | wc -c)"; }
    printf 'Cookie: <session cookies from cookie jar>\n'
    [[ -n "$body" ]] && { printf '\n%s\n' "$body"; } || printf '\n'
  } > "$dir/request.http"

  if [[ -n "$body" ]]; then
    curl -sS -A "$UA" -b "$COOKIE_JAR" -c "$COOKIE_JAR" \
      -H "Content-Type: application/json" -H "Origin: $BASE_URL" -H "Referer: $BASE_URL/" \
      -X "$method" -d "$body" -D "$dir/response-headers.txt" -o "$dir/body.raw" \
      "$BASE_URL$path"
  else
    curl -sS -A "$UA" -b "$COOKIE_JAR" -c "$COOKIE_JAR" \
      -H "Origin: $BASE_URL" -H "Referer: $BASE_URL/" \
      -X "$method" -D "$dir/response-headers.txt" -o "$dir/body.raw" \
      "$BASE_URL$path"
  fi

  # Keep a stable, diffable subset of response headers.
  {
    head -1 "$dir/response-headers.txt" | tr -d '\r'
    grep -iE '^(content-type|allow|location|x-robots-tag):' "$dir/response-headers.txt" | tr -d '\r' | sort -f
    printf '\n'
  } > "$dir/response.http"

  if python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$dir/body.raw" 2>/dev/null; then
    python3 -c 'import json,sys; json.dump(json.load(open(sys.argv[1])), open(sys.argv[2],"w"), indent=2, sort_keys=True); open(sys.argv[2],"a").write("\n")' \
      "$dir/body.raw" "$dir/body.json"
    rm -f "$dir/body.raw"
    status="$(head -1 "$dir/response-headers.txt" | tr -d '\r')"
    echo "$name: $status (json)"
  else
    status="$(head -1 "$dir/response-headers.txt" | tr -d '\r')"
    echo "$name: $status (raw)"
  fi
}

capture "web-root" GET "/"
capture "web-robots" GET "/robots.txt"
capture "users-me" GET "/api/users/me/"
capture "users-me-workspaces" GET "/api/users/me/workspaces/"
capture "workspaces-root" GET "/api/workspaces/"
capture "workspace-detail" GET "/api/workspaces/$WORKSPACE_SLUG/"
capture "project-list" GET "/api/workspaces/$WORKSPACE_SLUG/projects/"
capture "project-detail" GET "/api/workspaces/$WORKSPACE_SLUG/projects/$PROJECT_ID/"
capture "issue-list" GET "/api/workspaces/$WORKSPACE_SLUG/projects/$PROJECT_ID/issues/"
capture "issue-detail" GET "/api/workspaces/$WORKSPACE_SLUG/projects/$PROJECT_ID/issues/$ISSUE_ID/"
capture "state-list" GET "/api/workspaces/$WORKSPACE_SLUG/projects/$PROJECT_ID/states/"
capture "label-list" GET "/api/workspaces/$WORKSPACE_SLUG/projects/$PROJECT_ID/issue-labels/"
capture "member-list" GET "/api/workspaces/$WORKSPACE_SLUG/members/"
capture "cycle-list" GET "/api/workspaces/$WORKSPACE_SLUG/projects/$PROJECT_ID/cycles/"

echo "golden capture complete -> $GOLDEN_DIR"