#!/bin/sh
HOST="${RABBITMQ_HOST:-127.0.0.1}"
PORT="${RABBITMQ_PORT:-15672}"
INTERVAL="${INTERVAL:-1}"

pip install pika --quiet

python3 -c "
import pika, time, os, random

host = os.environ.get('RABBITMQ_HOST', '127.0.0.1')
port = int(os.environ.get('RABBITMQ_PORT', '15672'))
interval = float(os.environ.get('INTERVAL', '1'))

print('Waiting for RabbitMQ via proxy...')
while True:
    try:
        conn = pika.BlockingConnection(pika.ConnectionParameters(host=host, port=port))
        break
    except Exception:
        time.sleep(2)

ch = conn.channel()
queues = ['tasks', 'events', 'logs']
exchanges = ['topic_ex', 'fanout_ex']

ch.exchange_declare(exchange='topic_ex', exchange_type='topic')
ch.exchange_declare(exchange='fanout_ex', exchange_type='fanout')
for q in queues:
    ch.queue_declare(queue=q)
ch.queue_bind(queue='tasks', exchange='topic_ex', routing_key='task.*')
ch.queue_bind(queue='events', exchange='fanout_ex')

# Consumer callback
def on_message(ch, method, properties, body):
    print(f'Consumed: {body.decode()} from {method.routing_key}')
    ch.basic_ack(delivery_tag=method.delivery_tag)

# Subscribe to 'events' queue
ch.basic_consume(queue='events', on_message_callback=on_message)

print('Starting AMQP operations...')
i = 0
while True:
    op = i % 5
    q = queues[i % len(queues)]
    try:
        if op == 0:
            ch.basic_publish(exchange='', routing_key=q, body=f'msg_{i}_{int(time.time())}')
        elif op == 1:
            ch.basic_publish(exchange='topic_ex', routing_key=f'task.{i%10}', body=f'topic_msg_{i}')
        elif op == 2:
            ch.basic_publish(exchange='fanout_ex', routing_key='', body=f'broadcast_{i}')
        elif op == 3:
            method, props, body = ch.basic_get(queue=q, auto_ack=True)
            if body:
                print(f'Got: {body.decode()}')
        elif op == 4:
            ch.queue_purge(queue=q)
    except Exception as e:
        print(f'Error: {e}')
        while True:
            try:
                time.sleep(2)
                conn = pika.BlockingConnection(pika.ConnectionParameters(host=host, port=port))
                ch = conn.channel()
                ch.basic_consume(queue='events', on_message_callback=on_message)
                break
            except Exception:
                pass
    i += 1
    time.sleep(interval)
    try:
        conn.process_data_events(time_limit=0)
    except Exception:
        pass
"
