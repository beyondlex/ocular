#!/bin/sh
HOST="${PGHOST:-127.0.0.1}"
PORT="${PGPORT:-15432}"
USER="${PGUSER:-postgres}"
DB="${PGDATABASE:-testdb}"
INTERVAL="${INTERVAL:-1}"
export PGPASSWORD="${PGPASSWORD:-postgres}"

psql_cmd() {
  psql -h "$HOST" -p "$PORT" -U "$USER" -d "$DB" -c "$1" 2>/dev/null
}

echo "Waiting for PostgreSQL via proxy..."
until psql_cmd "SELECT 1" >/dev/null 2>&1; do
  sleep 2
done

psql_cmd "CREATE TABLE IF NOT EXISTS users (
  id SERIAL PRIMARY KEY,
  name VARCHAR(64),
  email VARCHAR(128),
  age INT,
  created_at TIMESTAMP DEFAULT NOW()
)"

i=0
while true; do
  op=$((i % 5))
  id=$((i % 50 + 1))
  case $op in
    0) psql_cmd "INSERT INTO users (name, email, age) VALUES ('user_$i', 'user_$i@test.com', $((i % 50 + 18)))" ;;
    1) psql_cmd "SELECT * FROM users WHERE id = $id" ;;
    2) psql_cmd "SELECT * FROM users ORDER BY RANDOM() LIMIT 5" ;;
    3) psql_cmd "UPDATE users SET age = $((i % 50 + 18)) WHERE id = $id" ;;
    4) psql_cmd "DELETE FROM users WHERE id = $id" ;;
  esac
  i=$((i + 1))
  sleep "$INTERVAL"
done
