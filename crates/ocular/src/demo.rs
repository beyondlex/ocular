use ocular_protocol::{ProxyEvent, Protocol};
use std::time::{Duration, SystemTime};
use tokio::sync::broadcast;

struct DemoEvent {
    component: &'static str,
    protocol: Protocol,
    command: &'static str,
    response: &'static str,
    latency_ms: f64,
}

const EVENTS: &[DemoEvent] = &[
    DemoEvent { component: "redis", protocol: Protocol::Redis, command: "SET user:1001 \"{\\\"name\\\":\\\"Alice\\\",\\\"email\\\":\\\"alice@example.com\\\"}\"", response: "OK", latency_ms: 0.42 },
    DemoEvent { component: "redis", protocol: Protocol::Redis, command: "GET user:1001", response: "{\"name\":\"Alice\",\"email\":\"alice@example.com\"}", latency_ms: 0.31 },
    DemoEvent { component: "redis", protocol: Protocol::Redis, command: "HSET session:abc token xyz expire 3600", response: "OK", latency_ms: 0.38 },
    DemoEvent { component: "redis", protocol: Protocol::Redis, command: "DEL cache:products:list", response: "(integer) 1", latency_ms: 0.25 },
    DemoEvent { component: "mysql", protocol: Protocol::Mysql, command: "SELECT id, name, email FROM users WHERE active = 1 ORDER BY created_at DESC LIMIT 20", response: "ResultSet (20 rows, 3 cols)", latency_ms: 3.21 },
    DemoEvent { component: "mysql", protocol: Protocol::Mysql, command: "INSERT INTO orders (user_id, product_id, quantity, total) VALUES (1001, 42, 2, 59.98)", response: "OK (1 row affected)", latency_ms: 1.87 },
    DemoEvent { component: "mysql", protocol: Protocol::Mysql, command: "UPDATE products SET stock = stock - 2 WHERE id = 42", response: "OK (1 row affected)", latency_ms: 2.14 },
    DemoEvent { component: "postgres", protocol: Protocol::Postgres, command: "SELECT o.id, u.name, p.title, o.total FROM orders o JOIN users u ON o.user_id = u.id JOIN products p ON o.product_id = p.id WHERE o.created_at > now() - interval '1 hour'", response: "ResultSet (5 rows)", latency_ms: 4.56 },
    DemoEvent { component: "postgres", protocol: Protocol::Postgres, command: "BEGIN; INSERT INTO audit_log (action, user_id, detail) VALUES ('purchase', 1001, 'order #8832'); COMMIT", response: "COMMIT", latency_ms: 2.03 },
    DemoEvent { component: "rabbitmq", protocol: Protocol::Amqp, command: "Basic.Publish exchange=events routing_key=order.created", response: "ACK", latency_ms: 1.12 },
    DemoEvent { component: "rabbitmq", protocol: Protocol::Amqp, command: "Basic.Publish exchange=notifications routing_key=email.send", response: "ACK", latency_ms: 0.98 },
    DemoEvent { component: "mongodb", protocol: Protocol::Mongodb, command: "db.analytics.insertOne({event: \"page_view\", path: \"/products/42\", user: 1001, ts: ISODate()})", response: "OK (insertedId: ObjectId(\"6651a...\"))", latency_ms: 1.45 },
    DemoEvent { component: "mongodb", protocol: Protocol::Mongodb, command: "db.products.find({category: \"electronics\", price: {$lt: 100}}).sort({rating: -1}).limit(10)", response: "10 documents", latency_ms: 3.78 },
    DemoEvent { component: "elasticsearch", protocol: Protocol::Http, command: "POST /products/_search {\"query\":{\"match\":{\"title\":\"wireless headphones\"}}}", response: "200 OK (hits: 12)", latency_ms: 15.32 },
    DemoEvent { component: "elasticsearch", protocol: Protocol::Http, command: "PUT /products/_doc/42 {\"title\":\"Wireless Headphones\",\"price\":49.99,\"stock\":98}", response: "200 OK", latency_ms: 8.67 },
    DemoEvent { component: "redis", protocol: Protocol::Redis, command: "INCR stats:page_views:2026-05-24", response: "(integer) 4832", latency_ms: 0.19 },
    DemoEvent { component: "redis", protocol: Protocol::Redis, command: "ZADD leaderboard 9500 user:1001", response: "(integer) 0", latency_ms: 0.33 },
    DemoEvent { component: "mysql", protocol: Protocol::Mysql, command: "SELECT COUNT(*) as total FROM orders WHERE created_at >= CURDATE()", response: "ResultSet (1 row, 1 col)", latency_ms: 1.54 },
];

pub async fn run_demo(tx: broadcast::Sender<ProxyEvent>) {
    let mut idx = 0;
    loop {
        let evt = &EVENTS[idx % EVENTS.len()];
        let _ = tx.send(ProxyEvent {
            timestamp: SystemTime::now(),
            component: evt.component.to_string(),
            protocol: evt.protocol,
            command: evt.command.to_string(),
            full_command: evt.command.to_string(),
            response: evt.response.to_string(),
            response_detail: evt.response.to_string(),
            latency: Duration::from_secs_f64(evt.latency_ms / 1000.0),
            process: None,
            src: Some("127.0.0.1:52431".to_string()),
            dest: Some("127.0.0.1:6379".to_string()),
        });
        idx += 1;
        tokio::time::sleep(Duration::from_millis(300 + (idx as u64 * 37) % 400)).await;
    }
}

pub fn demo_components() -> Vec<ocular_tui::ComponentInfo> {
    ["redis", "mysql", "postgres", "rabbitmq", "mongodb", "elasticsearch"]
        .iter()
        .map(|name| ocular_tui::ComponentInfo {
            name: name.to_string(),
            listen: String::new(),
            exclude: None,
            include: None,
        })
        .collect()
}
