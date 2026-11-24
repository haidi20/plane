sleep 20
echo "--- api log ---"
docker logs --tail 25 community-api-1 2>&1
echo "--- codes ---"
for p in /api/ /auth/ /api/instances/; do code=$(curl -s -o /dev/null -w "%{http_code}" --max-time 8 "http://127.0.0.1:8080$p"); echo "$p -> $code"; done