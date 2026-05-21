#!/bin/sh
HOST="${REDIS_HOST:-127.0.0.1}"
PORT="${REDIS_PORT:-16379}"
INTERVAL="${INTERVAL:-1}"

i=0
while true; do
  key="user:$((i % 100))"
  op=$((i % 5))
  case $op in
    0) redis-cli -h "$HOST" -p "$PORT" SET "$key" "name_$(date +%s)_$i" EX 300 ;;
    1) redis-cli -h "$HOST" -p "$PORT" GET "$key" ;;
    2) redis-cli -h "$HOST" -p "$PORT" HSET "$key:profile" age $((i % 60 + 18)) city "city_$((i % 10))" ;;
    3) redis-cli -h "$HOST" -p "$PORT" HGETALL "$key:profile" ;;
    4) redis-cli -h "$HOST" -p "$PORT" DEL "$key" ;;
  esac
  i=$((i + 1))
  sleep "$INTERVAL"
done
