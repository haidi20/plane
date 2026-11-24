echo "--- processes ---"
docker exec community-api-1 ps aux 2>&1 | head -12
echo "--- listening ---"
docker exec community-api-1 sh -c "cat /proc/net/tcp | awk 'NR>1 && \$4==\"0A\" {print \$2}' | head -10" 2>&1
echo "--- log full tail 60 ---"
docker logs --tail 60 community-api-1 2>&1 | tail -30