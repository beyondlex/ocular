#!/bin/sh
# This client connects to Ocular proxy (HTTP) which forwards to nginx (HTTPS)
HOST="${TARGET_HOST:-127.0.0.1}"
PORT="${TARGET_PORT:-18443}"
INTERVAL="${INTERVAL:-2}"

BASE="http://${HOST}:${PORT}"

echo "Waiting for HTTPS proxy..."
until curl -s "${BASE}/" >/dev/null 2>&1; do
  sleep 2
done

echo "Starting HTTPS proxy test..."
i=0
while true; do
  op=$((i % 3))
  case $op in
    0) curl -s "${BASE}/" >/dev/null ;;
    1) curl -s "${BASE}/api/users" >/dev/null ;;
    2) curl -s "${BASE}/api/health" >/dev/null ;;
  esac
  i=$((i + 1))
  sleep "$INTERVAL"
done
