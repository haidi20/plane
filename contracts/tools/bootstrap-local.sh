#!/usr/bin/env bash
# Copyright (c) 2023-present Plane Software, Inc. and contributors
# SPDX-License-Identifier: AGPL-3.0-only
# See the LICENSE file for details.

# Bootstrap a local Community instance through the real HTTP surface.
# This intentionally does not use the Django ORM or test client.
set -euo pipefail

BASE_URL="${BASE_URL:-http://localhost:8080}"
EMAIL="${EMAIL:-admin@plane.local}"
PASSWORD="${PASSWORD:-PlaneAdmin!2345}"
FIRST_NAME="${FIRST_NAME:-Plane}"
LAST_NAME="${LAST_NAME:-Admin}"
COMPANY_NAME="${COMPANY_NAME:-Plane}"
COOKIE_JAR="${COOKIE_JAR:-/tmp/plane-cookies.txt}"
UA="${UA:-Mozilla/5.0 (compatible; planesync/1.0)}"

csrf_token() {
  curl -fsS -A "$UA" -c "$COOKIE_JAR" "$BASE_URL/auth/get-csrf-token/" |
    python3 -c 'import json, sys; print(json.load(sys.stdin)["csrf_token"])'
}

post_form() {
  local url="$1"
  local token="$2"
  shift 2
  curl -sS -o /tmp/plane-bootstrap-body -D /tmp/plane-bootstrap-headers \
    -w '%{http_code}' \
    -A "$UA" \
    -b "$COOKIE_JAR" -c "$COOKIE_JAR" \
    -H "X-CSRFToken: $token" \
    -H "Origin: $BASE_URL" \
    -H "Referer: $BASE_URL/" \
    -X POST "$url" "$@"
}

rm -f "$COOKIE_JAR"

# NOTE: `is_telemetry_enabled` is intentionally omitted. The view defaults it to
# the boolean True; sending the string "false" makes Django's BooleanField
# validation fail and the endpoint returns HTTP 500.
signup_code=$(post_form "$BASE_URL/api/instances/admins/sign-up/" "$(csrf_token)" \
  --data-urlencode "email=$EMAIL" \
  --data-urlencode "password=$PASSWORD" \
  --data-urlencode "first_name=$FIRST_NAME" \
  --data-urlencode "last_name=$LAST_NAME" \
  --data-urlencode "company_name=$COMPANY_NAME")

if [[ "$signup_code" != "302" ]]; then
  echo "admin signup failed: HTTP $signup_code" >&2
  cat /tmp/plane-bootstrap-body >&2
  exit 1
fi

signin_code=$(post_form "$BASE_URL/auth/sign-in/" "$(csrf_token)" \
  --data-urlencode "email=$EMAIL" \
  --data-urlencode "password=$PASSWORD")

if [[ "$signin_code" != "302" ]]; then
  echo "app signin failed: HTTP $signin_code" >&2
  cat /tmp/plane-bootstrap-body >&2
  exit 1
fi

me_code=$(curl -sS -A "$UA" -o /tmp/plane-me.json -w '%{http_code}' -b "$COOKIE_JAR" "$BASE_URL/api/users/me/")
if [[ "$me_code" != "200" ]]; then
  echo "GET /api/users/me/ failed: HTTP $me_code" >&2
  cat /tmp/plane-me.json >&2
  exit 1
fi

python3 -c 'import json; d=json.load(open("/tmp/plane-me.json")); print(json.dumps({"email": d.get("email"), "id": d.get("id")}))'
echo "bootstrap ok; cookie jar: $COOKIE_JAR"