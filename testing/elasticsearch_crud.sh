#!/bin/sh
HOST="${ES_HOST:-127.0.0.1}"
PORT="${ES_PORT:-19200}"
INTERVAL="${INTERVAL:-1}"

BASE="http://${HOST}:${PORT}"

echo "Waiting for Elasticsearch via proxy..."
until curl -s "${BASE}/_cluster/health" >/dev/null 2>&1; do
  sleep 2
done

# Create index
curl -s -X PUT "${BASE}/users" -H 'Content-Type: application/json' -d '{
  "mappings": {"properties": {"name": {"type": "text"}, "email": {"type": "keyword"}, "age": {"type": "integer"}}}
}' >/dev/null

echo "Starting Elasticsearch operations..."
i=0
while true; do
  op=$((i % 5))
  id=$((i % 50 + 1))
  case $op in
    0) curl -s -X POST "${BASE}/users/_doc/${id}" -H 'Content-Type: application/json' \
         -d "{\"name\":\"user_${i}\",\"email\":\"user_${i}@test.com\",\"age\":$((i % 50 + 18))}" >/dev/null ;;
    1) curl -s "${BASE}/users/_doc/${id}" >/dev/null ;;
    2) curl -s -X POST "${BASE}/users/_search" -H 'Content-Type: application/json' \
         -d '{"query":{"range":{"age":{"gte":30}}},"size":5}' >/dev/null ;;
    3) curl -s -X POST "${BASE}/users/_update/${id}" -H 'Content-Type: application/json' \
         -d "{\"doc\":{\"age\":$((i % 50 + 18))}}" >/dev/null ;;
    4) curl -s -X DELETE "${BASE}/users/_doc/${id}" >/dev/null ;;
  esac
  i=$((i + 1))
  sleep "$INTERVAL"
done
