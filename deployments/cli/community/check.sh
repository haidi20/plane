cd /mnt/d/projects/plane/deployments/cli/community
echo "restart counts:"
docker ps -a --filter "name=community-" --format "{{.Names}} {{.Status}}" | sort
echo "--- http codes ---"
for p in / /god-mode/ /spaces/ /api/ /auth/; do code=$(curl -s -o /dev/null -w "%{http_code}" --max-time 6 "http://127.0.0.1:8080$p"); echo "$p -> $code"; done
echo "--- title ---"
curl -s --max-time 8 http://127.0.0.1:8080/ | grep -o "<title>[^<]*</title>" | head -3