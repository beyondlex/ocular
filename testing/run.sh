#!/bin/sh
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
COMPOSE="docker compose -f $SCRIPT_DIR/docker-compose.yml"

start() {
  mkdir -p "$PID_DIR"
  echo "Starting backend services (Redis + MySQL)..."
  $COMPOSE up -d

  echo "Waiting for MySQL to be ready..."
  until $COMPOSE exec -T mysql mysqladmin ping -uroot -proot --silent 2>/dev/null; do
    sleep 1
  done

  echo "Waiting for PostgreSQL to be ready..."
  until $COMPOSE exec -T postgres psql -U postgres -d testdb -c "SELECT 1" >/dev/null 2>&1; do
    sleep 1
  done

  if [ "${1:-}" != "--no-client" ]; then
    echo "Starting client containers..."
    $COMPOSE --profile client up -d
  fi

  echo "Running. Use '$0 stop' to stop."
}

stop() {
  echo "Stopping all services..."
  $COMPOSE --profile client down
  echo "Stopped."
}

case "${1:-}" in
  start) start "$2" ;;
  stop)  stop ;;
  *)     echo "Usage: $0 {start [--no-client]|stop}" ;;
esac
