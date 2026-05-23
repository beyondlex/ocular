#!/bin/sh
HOST="${MONGO_HOST:-127.0.0.1}"
PORT="${MONGO_PORT:-17017}"
INTERVAL="${INTERVAL:-1}"

echo "Waiting for MongoDB via proxy..."
until mongosh --host "$HOST" --port "$PORT" --eval "db.runCommand({ping:1})" --quiet 2>/dev/null; do
  sleep 2
done

echo "Starting MongoDB operations..."
mongosh --host "$HOST" --port "$PORT" --eval "
  const db = db.getSiblingDB('testdb');
  let i = 0;
  while (true) {
    const op = i % 5;
    const id = i % 50 + 1;
    try {
      if (op === 0) {
        db.users.insertOne({name: 'user_' + i, email: 'user_' + i + '@test.com', age: (i % 50) + 18});
      } else if (op === 1) {
        db.users.findOne({name: 'user_' + id});
      } else if (op === 2) {
        db.users.find({age: {\$gte: 30}}).limit(5).toArray();
      } else if (op === 3) {
        db.users.updateOne({name: 'user_' + id}, {\$set: {age: (i % 50) + 18}});
      } else if (op === 4) {
        db.users.deleteOne({name: 'user_' + id});
      }
    } catch (e) {
      print('Error: ' + e.message);
    }
    i++;
    sleep(${INTERVAL} * 1000);
  }
"
