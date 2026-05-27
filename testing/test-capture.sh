#!/bin/bash
# test-capture.sh — Generate traffic to all Docker services for capture testing.
# Usage: ./test-capture.sh [loops]
# All services on 127.0.0.1 via Docker port mapping.

LOOPS=${1:-3}
SLEEP=0.5

echo "=== Generating traffic ($LOOPS iterations) ==="

for i in $(seq 1 $LOOPS); do
  echo "--- Round $i/$LOOPS ---"

  # Redis (no SSL needed)
  redis-cli -h 127.0.0.1 -p 6379 SET "user:$i" "hello-$i" > /dev/null 2>&1
  redis-cli -h 127.0.0.1 -p 6379 GET "user:$i" > /dev/null 2>&1
  redis-cli -h 127.0.0.1 -p 6379 HSET "session:$i" name "test" age "$i" > /dev/null 2>&1
  redis-cli -h 127.0.0.1 -p 6379 EXPIRE "user:$i" 60 > /dev/null 2>&1

  # MySQL (SSL disabled — required for capture to see plaintext)
  mysql -h 127.0.0.1 -P 3306 -u root -proot --ssl-mode=DISABLED -e "
    SELECT NOW();
    USE testdb;
    CREATE TABLE IF NOT EXISTS capture_test (id INT AUTO_INCREMENT PRIMARY KEY, val VARCHAR(100), ts TIMESTAMP DEFAULT CURRENT_TIMESTAMP);
    INSERT INTO capture_test (val) VALUES ('round-$i');
    SELECT * FROM capture_test ORDER BY id DESC LIMIT 5;
  " 2>/dev/null

  # PostgreSQL (no SSL by default with trust auth)
  PGPASSWORD=postgres psql -h 127.0.0.1 -p 5432 -U postgres -d testdb -c "
    CREATE TABLE IF NOT EXISTS capture_test (id SERIAL PRIMARY KEY, val TEXT, ts TIMESTAMP DEFAULT NOW());
    INSERT INTO capture_test (val) VALUES ('round-$i');
    SELECT * FROM capture_test ORDER BY id DESC LIMIT 5;
  " > /dev/null 2>&1

  # MongoDB (no auth, no SSL)
  mongosh --host 127.0.0.1 --port 27017 --quiet --eval "
    db = db.getSiblingDB('testdb');
    db.capture_test.insertOne({val: 'round-$i', ts: new Date()});
    db.capture_test.find().sort({_id: -1}).limit(5).toArray();
  " > /dev/null 2>&1

  # RabbitMQ (AMQP, default guest/guest)
  python3 -c "
import pika
conn = pika.BlockingConnection(pika.ConnectionParameters('127.0.0.1'))
ch = conn.channel()
ch.queue_declare(queue='capture-test')
ch.basic_publish(exchange='', routing_key='capture-test', body='message-$i')
conn.close()
" 2>/dev/null

  # Memcached (no auth, no SSL)
  (echo -e "set key$i 0 60 7\r\nround-$i\r"; sleep 0.2) | nc 127.0.0.1 11211 > /dev/null 2>&1
  (echo -e "get key$i\r"; sleep 0.2) | nc 127.0.0.1 11211 > /dev/null 2>&1

  # Elasticsearch / HTTP (no auth)
  curl -s -X PUT "http://127.0.0.1:9200/capture-test/_doc/$i" \
    -H "Content-Type: application/json" \
    -d "{\"val\":\"round-$i\",\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"}" > /dev/null 2>&1
  curl -s "http://127.0.0.1:9200/capture-test/_search?size=3" > /dev/null 2>&1

  # Kafka (no auth) — uses kafka-capture service on port 9192 (advertised as 127.0.0.1:9192)
  if command -v kcat &> /dev/null; then
    echo "message-$i" | kcat -P -b 127.0.0.1:9192 -t capture-test 2>/dev/null
    kcat -C -b 127.0.0.1:9192 -t capture-test -c 1 -o end 2>/dev/null
  elif command -v kafka-console-producer.sh &> /dev/null; then
    echo "message-$i" | kafka-console-producer.sh --broker-list 127.0.0.1:9192 --topic capture-test 2>/dev/null
  fi

  sleep $SLEEP
done

echo "=== Done. $LOOPS rounds of traffic generated. ==="
