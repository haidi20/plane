for img in makeplane/plane-backend:stable makeplane/plane-frontend:stable makeplane/plane-space:stable makeplane/plane-live:stable makeplane/plane-proxy:stable postgres:15.7-alpine valkey/valkey:7.2.11-alpine rabbitmq:3.13.6-management-alpine pgsty/silo:RELEASE.2026-09-16T00-00-00Z; do
  echo "=== $img"
  docker pull -q "$img" || echo "FAIL $img"
done
echo DONE
docker images --format "{{.Repository}}:{{.Tag}} {{.Size}}"