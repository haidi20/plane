cd /mnt/d/projects/plane/deployments/cli/community
docker compose ps --format "{{.Name}} {{.State}} {{.Status}}"
echo "--- proxy log ---"
docker logs --tail 15 community-proxy-1 2>&1
echo "--- curl try ---"
for i in 1 2 3; do curl -s -o /dev/null -w "%{http_code}\n" --max-time 5 http://127.0.0.1:8080/; sleep 2; done