#!/bin/sh
BROKER="${KAFKA_BROKER:-host.docker.internal:19092}"
TOPIC="${KAFKA_TOPIC:-test-events}"
INTERVAL="${INTERVAL:-1}"

# Wait for Kafka to be ready
echo "Waiting for Kafka..."
sleep 10

# Create topic
kafka-topics.sh --bootstrap-server "$BROKER" --create --topic "$TOPIC" --partitions 3 --replication-factor 1 --if-not-exists 2>/dev/null

i=0
while true; do
  op=$((i % 3))
  case $op in
    0)
      echo "{\"event\":\"order.created\",\"user\":$((i % 100)),\"total\":$((i * 7 % 500))}" | \
        kafka-console-producer.sh --bootstrap-server "$BROKER" --topic "$TOPIC" 2>/dev/null
      ;;
    1)
      kafka-console-consumer.sh --bootstrap-server "$BROKER" --topic "$TOPIC" --max-messages 1 --timeout-ms 2000 2>/dev/null
      ;;
    2)
      kafka-topics.sh --bootstrap-server "$BROKER" --describe --topic "$TOPIC" 2>/dev/null
      ;;
  esac
  i=$((i + 1))
  sleep "$INTERVAL"
done
