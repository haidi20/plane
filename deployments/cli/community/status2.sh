for i in $(seq 1 40); do
  st=$(docker inspect -f '{{.State.Status}}' community-migrator-1)
  if [ "$st" != "running" ]; then echo "migrator finished: $st"; break; fi
  sleep 10
done
echo "--- migrator last ---"
docker logs --tail 4 community-migrator-1 2>&1
echo "--- api last ---"
docker logs --tail 6 community-api-1 2>&1
echo "--- http ---"
curl -s -o /dev/null -w "%{http_code}\n" http://localhost:8080/