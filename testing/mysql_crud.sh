#!/bin/sh
HOST="${MYSQL_HOST:-127.0.0.1}"
PORT="${MYSQL_PORT:-13306}"
USER="${MYSQL_USER:-root}"
PASS="${MYSQL_PASS:-root}"
DB="${MYSQL_DB:-testdb}"
INTERVAL="${INTERVAL:-1}"

mysql_cmd() {
  mysql -h "$HOST" -P "$PORT" -u "$USER" -p"$PASS" --ssl-mode=DISABLED "$DB" -e "$1" 2>/dev/null
}

# Wait for MySQL to be reachable via ocular proxy
echo "Waiting for MySQL via proxy..."
until mysql_cmd "SELECT 1" >/dev/null 2>&1; do
  sleep 2
done

mysql_cmd "CREATE TABLE IF NOT EXISTS users (
  id INT AUTO_INCREMENT PRIMARY KEY,
  name VARCHAR(64),
  email VARCHAR(128),
  age INT,
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
)"

i=0
while true; do
  op=$((i % 5))
  id=$((i % 50 + 1))
  case $op in
    0) mysql_cmd "INSERT INTO users (name, email, age) VALUES ('user_$i', 'user_$i@test.com', $((i % 50 + 18)))" ;;
    1) mysql_cmd "SELECT * FROM users WHERE id = $id" ;;
    2) mysql_cmd "SELECT * FROM users ORDER BY RAND() LIMIT 5" ;;
    3) mysql_cmd "UPDATE users SET age = $((i % 50 + 18)) WHERE id = $id" ;;
    4) mysql_cmd "DELETE FROM users WHERE id = $id" ;;
  esac
  i=$((i + 1))
  sleep "$INTERVAL"
done
