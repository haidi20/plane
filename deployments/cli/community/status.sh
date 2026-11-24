for i in 1 2 3 4 5 6 7 8 9 10 11 12; do
  st=$(docker inspect -f '{{.State.Status}}' community-migrator-1)
  echo "migrator=$st"
  if [ "$st" != "running" ]; then break; fi
  sleep 10
done
echo "--- migrator tail ---"
docker logs --tail 6 community-migrator-1 2>&1
echo "--- api tail ---"
docker logs --tail 4 community-api-1 2>&1
echo "--- http ---"
curl -s -o /dev/null -w "%{http_code}\n" http://localhost:8080/ || true