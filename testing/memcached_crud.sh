#!/bin/sh
HOST="${MEMCACHED_HOST:-127.0.0.1}"
PORT="${MEMCACHED_PORT:-11212}"
INTERVAL="${INTERVAL:-1}"

i=0
while true; do
  key="user:$((i % 100))"
  op=$((i % 5))
  case $op in
    0) printf "set %s 0 300 %d\r\n%s\r\n" "$key" "$(printf '%s' "name_$(date +%s)_$i" | wc -c | tr -d ' ')" "name_$(date +%s)_$i" | nc -w 2 "$HOST" "$PORT" ;;
    1) printf "get %s\r\n" "$key" | nc -w 2 "$HOST" "$PORT" ;;
    2) printf "incr counter_%s 1\r\n" "$((i % 10))" | nc -w 2 "$HOST" "$PORT" ;;
    3) printf "delete %s\r\n" "$key" | nc -w 2 "$HOST" "$PORT" ;;
    4) printf "gets %s %s\r\n" "$key" "user:$((i % 50 + 100))" | nc -w 2 "$HOST" "$PORT" ;;
  esac
  i=$((i + 1))
  sleep "$INTERVAL"
done
