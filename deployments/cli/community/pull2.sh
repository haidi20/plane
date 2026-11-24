set -e
for img in makeplane/plane-live:stable makeplane/plane-proxy:stable postgres:15.7-alpine pgsty/silo:RELEASE.2026-09-16T00-00-00Z; do
  echo "=== $img"
  docker pull -q "$img" || echo "FAIL $img"
done
echo PULL_DONE
docker images --format "{{.Repository}}:{{.Tag}}" | sort