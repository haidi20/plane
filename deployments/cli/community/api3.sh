for i in $(seq 1 18); do
  code=$(curl -s -o /dev/null -w "%{http_code}" --max-time 6 http://127.0.0.1:8080/api/)
  echo "try $i: /api/ -> $code"
  if [ "$code" != "502" ] && [ "$code" != "000" ]; then break; fi
  sleep 8
done
echo "--- api tail ---"
docker logs --tail 6 community-api-1 2>&1
echo "--- proc ---"
docker exec community-api-1 ps aux 2>&1 | head -8