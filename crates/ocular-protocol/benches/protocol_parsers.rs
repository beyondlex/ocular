use criterion::{black_box, criterion_group, criterion_main, Criterion};
use ocular_protocol::{resp, mysql, postgres, mongodb, http, memcached, kafka, amqp};

// ─── RESP (Redis) ──────────────────────────────────────────────────────

fn bench_resp(c: &mut Criterion) {
    let mut group = c.benchmark_group("resp");

    // Simple string
    let simple = b"+OK\r\n";
    group.bench_function("simple_string", |b| {
        b.iter(|| resp::parse_resp(black_box(simple)))
    });

    // Integer
    let integer = b":42\r\n";
    group.bench_function("integer", |b| {
        b.iter(|| resp::parse_resp(black_box(integer)))
    });

    // Bulk string
    let bulk = b"$5\r\nhello\r\n";
    group.bench_function("bulk_string", |b| {
        b.iter(|| resp::parse_resp(black_box(bulk)))
    });

    // Array (SET command)
    let array = b"*3\r\n$3\r\nSET\r\n$3\r\nkey\r\n$5\r\nvalue\r\n";
    group.bench_function("array_set", |b| {
        b.iter(|| resp::parse_resp(black_box(array)))
    });

    // Large array (HMSET with 10 fields)
    let mut large_array = Vec::new();
    large_array.extend_from_slice(b"*21\r\n$5\r\nHMSET\r\n$4\r\nhash\r\n");
    for i in 0..10 {
        large_array.extend_from_slice(format!("$5\r\nfield\r\n$6\r\nvalue{}\r\n", i).as_bytes());
    }
    group.bench_function("large_array_hmset", |b| {
        b.iter(|| resp::parse_resp(black_box(&large_array)))
    });

    // Nested array
    let nested = b"*2\r\n*2\r\n$1\r\na\r\n$1\r\nb\r\n*2\r\n$1\r\nc\r\n$1\r\nd\r\n";
    group.bench_function("nested_array", |b| {
        b.iter(|| resp::parse_resp(black_box(nested)))
    });

    group.finish();
}

// ─── MySQL ─────────────────────────────────────────────────────────────

fn bench_mysql(c: &mut Criterion) {
    let mut group = c.benchmark_group("mysql");

    // Simple SELECT
    let simple_query = b"SELECT 1";
    let mut simple_pkt = vec![(simple_query.len() + 1) as u8, 0, 0, 0, 0x03];
    simple_pkt.extend_from_slice(simple_query);
    group.bench_function("simple_query", |b| {
        b.iter(|| mysql::parse_mysql_request(black_box(&simple_pkt)))
    });

    // Complex SELECT with joins
    let complex_query = b"SELECT u.id, u.name, o.total FROM users u JOIN orders o ON u.id = o.user_id WHERE u.active = 1 ORDER BY o.created_at DESC LIMIT 100";
    let mut complex_pkt = vec![(complex_query.len() + 1) as u8, 0, 0, 0, 0x03];
    complex_pkt.extend_from_slice(complex_query);
    group.bench_function("complex_query", |b| {
        b.iter(|| mysql::parse_mysql_request(black_box(&complex_pkt)))
    });

    // OK response
    let ok_response = vec![7, 0, 0, 1, 0x00, 0, 0, 0x02, 0, 0, 0];
    group.bench_function("ok_response", |b| {
        b.iter(|| mysql::parse_mysql_response(black_box(&ok_response)))
    });

    // Error response
    let mut error_payload = vec![0xff];
    error_payload.extend_from_slice(&[0x01, 0x00]);
    error_payload.push(b'#');
    error_payload.extend_from_slice(b"HY000");
    error_payload.extend_from_slice(b"test error");
    let len = error_payload.len() as u32;
    let mut error_pkt = vec![(len & 0xff) as u8, ((len >> 8) & 0xff) as u8, ((len >> 16) & 0xff) as u8, 0x01];
    error_pkt.extend_from_slice(&error_payload);
    group.bench_function("error_response", |b| {
        b.iter(|| mysql::parse_mysql_response(black_box(&error_pkt)))
    });

    // Response completeness check
    group.bench_function("response_complete_ok", |b| {
        b.iter(|| mysql::mysql_response_complete(black_box(&ok_response)))
    });

    group.finish();
}

// ─── PostgreSQL ────────────────────────────────────────────────────────

fn bench_postgres(c: &mut Criterion) {
    let mut group = c.benchmark_group("postgres");

    // Simple query
    let sql = b"SELECT 1\0";
    let len = (sql.len() as u32 + 4).to_be_bytes();
    let mut query_buf = vec![b'Q'];
    query_buf.extend_from_slice(&len);
    query_buf.extend_from_slice(sql);
    group.bench_function("simple_query", |b| {
        b.iter(|| postgres::parse_postgres_request(black_box(&query_buf)))
    });

    // Complex query
    let complex_sql = b"SELECT u.id, u.name, o.total FROM users u JOIN orders o ON u.id = o.user_id WHERE u.active = 1 ORDER BY o.created_at DESC LIMIT 100\0";
    let len = (complex_sql.len() as u32 + 4).to_be_bytes();
    let mut complex_buf = vec![b'Q'];
    complex_buf.extend_from_slice(&len);
    complex_buf.extend_from_slice(complex_sql);
    group.bench_function("complex_query", |b| {
        b.iter(|| postgres::parse_postgres_request(black_box(&complex_buf)))
    });

    // CommandComplete response
    let tag = b"INSERT 0 1\0";
    let len = (tag.len() as u32 + 4).to_be_bytes();
    let mut resp_buf = vec![b'C'];
    resp_buf.extend_from_slice(&len);
    resp_buf.extend_from_slice(tag);
    group.bench_function("command_complete", |b| {
        b.iter(|| postgres::parse_postgres_response(black_box(&resp_buf)))
    });

    // ReadyForQuery
    let mut ready_buf = vec![b'Z'];
    ready_buf.extend_from_slice(&5u32.to_be_bytes());
    ready_buf.push(b'I');
    group.bench_function("ready_for_query", |b| {
        b.iter(|| postgres::parse_postgres_response(black_box(&ready_buf)))
    });

    // Response completeness
    group.bench_function("response_complete", |b| {
        b.iter(|| postgres::postgres_response_complete(black_box(&ready_buf)))
    });

    group.finish();
}

// ─── MongoDB BSON ──────────────────────────────────────────────────────

fn bench_mongodb(c: &mut Criterion) {
    let mut group = c.benchmark_group("mongodb");

    // Build a simple OP_MSG with find command
    fn build_op_msg(doc: &[u8]) -> Vec<u8> {
        let msg_len = 16 + 4 + 1 + doc.len();
        let mut buf = Vec::new();
        buf.extend_from_slice(&(msg_len as i32).to_le_bytes());
        buf.extend_from_slice(&1i32.to_le_bytes());
        buf.extend_from_slice(&0i32.to_le_bytes());
        buf.extend_from_slice(&2013i32.to_le_bytes()); // OP_MSG
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.push(0); // kind 0
        buf.extend_from_slice(doc);
        buf
    }

    fn build_simple_cmd(cmd: &str, coll: &str) -> Vec<u8> {
        let mut doc = Vec::new();
        doc.extend_from_slice(&[0; 4]);
        doc.push(0x02);
        doc.extend_from_slice(cmd.as_bytes());
        doc.push(0);
        let val = format!("{}\0", coll);
        doc.extend_from_slice(&(val.len() as i32).to_le_bytes());
        doc.extend_from_slice(val.as_bytes());
        doc.push(0x02);
        doc.extend_from_slice(b"$db\0");
        let db = "testdb\0";
        doc.extend_from_slice(&(db.len() as i32).to_le_bytes());
        doc.extend_from_slice(db.as_bytes());
        doc.push(0);
        let len = doc.len() as i32;
        doc[0..4].copy_from_slice(&len.to_le_bytes());
        doc
    }

    // Simple find
    let find_doc = build_simple_cmd("find", "users");
    let find_buf = build_op_msg(&find_doc);
    group.bench_function("parse_find_request", |b| {
        b.iter(|| mongodb::parse_mongo_request(black_box(&find_buf)))
    });

    // Extract full command
    group.bench_function("extract_full_command", |b| {
        b.iter(|| mongodb::extract_mongo_full_command(black_box(&find_buf)))
    });

    // Parse response with ok field
    let mut ok_doc = Vec::new();
    ok_doc.extend_from_slice(&[0; 4]);
    ok_doc.push(0x01);
    ok_doc.extend_from_slice(b"ok\0");
    ok_doc.extend_from_slice(&1.0f64.to_le_bytes());
    ok_doc.push(0);
    let len = ok_doc.len() as i32;
    ok_doc[0..4].copy_from_slice(&len.to_le_bytes());
    let ok_buf = build_op_msg(&ok_doc);
    group.bench_function("parse_response_ok", |b| {
        b.iter(|| mongodb::parse_mongo_response(black_box(&ok_buf)))
    });

    // Message length extraction
    group.bench_function("mongo_msg_len", |b| {
        b.iter(|| mongodb::mongo_msg_len(black_box(&find_buf)))
    });

    // Large document with many fields
    let mut large_doc = Vec::new();
    large_doc.extend_from_slice(&[0; 4]);
    large_doc.push(0x02);
    large_doc.extend_from_slice(b"cmd\0");
    let cmd_val = "aggregate\0";
    large_doc.extend_from_slice(&(cmd_val.len() as i32).to_le_bytes());
    large_doc.extend_from_slice(cmd_val.as_bytes());
    for i in 0..20 {
        large_doc.push(0x02);
        let key = format!("field{}\0", i);
        large_doc.extend_from_slice(key.as_bytes());
        let val = format!("value{}\0", i);
        large_doc.extend_from_slice(&(val.len() as i32).to_le_bytes());
        large_doc.extend_from_slice(val.as_bytes());
    }
    large_doc.push(0);
    let len = large_doc.len() as i32;
    large_doc[0..4].copy_from_slice(&len.to_le_bytes());
    let large_buf = build_op_msg(&large_doc);
    group.bench_function("parse_large_document", |b| {
        b.iter(|| mongodb::parse_mongo_request(black_box(&large_buf)))
    });

    group.finish();
}

// ─── HTTP ──────────────────────────────────────────────────────────────

fn bench_http(c: &mut Criterion) {
    let mut group = c.benchmark_group("http");

    // Simple GET
    let get_req = b"GET /users/_doc/1 HTTP/1.1\r\nHost: localhost:9200\r\n\r\n";
    group.bench_function("parse_get_request", |b| {
        b.iter(|| http::parse_http_request(black_box(get_req)))
    });

    // POST with body
    let post_req = b"POST /users/_search HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"size\": 5}";
    group.bench_function("parse_post_request", |b| {
        b.iter(|| http::parse_http_request(black_box(post_req)))
    });

    // Extract full command
    let full_req = b"GET /index HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer token\r\n\r\n";
    group.bench_function("extract_full_command", |b| {
        b.iter(|| http::extract_http_full_command(black_box(full_req)))
    });

    // Response parsing
    let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
    group.bench_function("parse_response", |b| {
        b.iter(|| http::parse_http_response(black_box(resp)))
    });

    // Response completeness
    group.bench_function("response_complete", |b| {
        b.iter(|| http::http_response_complete(black_box(resp)))
    });

    // Chunked response
    let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
    group.bench_function("chunked_complete", |b| {
        b.iter(|| http::http_response_complete(black_box(chunked)))
    });

    group.finish();
}

// ─── Memcached ─────────────────────────────────────────────────────────

fn bench_memcached(c: &mut Criterion) {
    let mut group = c.benchmark_group("memcached");

    // GET
    let get = b"get user:1\r\n";
    group.bench_function("parse_get", |b| {
        b.iter(|| memcached::parse_memcached_request(black_box(get)))
    });

    // SET with data
    let set = b"set user:1 0 300 5\r\nhello\r\n";
    group.bench_function("parse_set", |b| {
        b.iter(|| memcached::parse_memcached_request(black_box(set)))
    });

    // Response
    let resp = b"VALUE user:1 0 5\r\nhello\r\nEND\r\n";
    group.bench_function("parse_response", |b| {
        b.iter(|| memcached::parse_memcached_response(black_box(resp)))
    });

    // Request completeness
    group.bench_function("request_complete", |b| {
        b.iter(|| memcached::memcached_request_complete(black_box(set)))
    });

    // Response completeness
    group.bench_function("response_complete", |b| {
        b.iter(|| memcached::memcached_response_complete(black_box(resp)))
    });

    group.finish();
}

// ─── Kafka ─────────────────────────────────────────────────────────────

fn bench_kafka(c: &mut Criterion) {
    let mut group = c.benchmark_group("kafka");

    // Build a Metadata request
    fn make_request(api_key: i16, api_version: i16, client_id: &str) -> Vec<u8> {
        let client_id_bytes = client_id.as_bytes();
        let payload_len = 2 + 2 + 4 + 2 + client_id_bytes.len();
        let mut buf = Vec::new();
        buf.extend_from_slice(&(payload_len as i32).to_be_bytes());
        buf.extend_from_slice(&api_key.to_be_bytes());
        buf.extend_from_slice(&api_version.to_be_bytes());
        buf.extend_from_slice(&1i32.to_be_bytes());
        buf.extend_from_slice(&(client_id_bytes.len() as i16).to_be_bytes());
        buf.extend_from_slice(client_id_bytes);
        buf
    }

    let metadata_req = make_request(3, 12, "my-app");
    group.bench_function("parse_metadata", |b| {
        b.iter(|| kafka::parse_kafka_request(black_box(&metadata_req)))
    });

    let api_versions_req = make_request(18, 3, "kafka-client");
    group.bench_function("parse_api_versions", |b| {
        b.iter(|| kafka::parse_kafka_request(black_box(&api_versions_req)))
    });

    // Response
    let mut resp = vec![0, 0, 0, 20];
    resp.extend_from_slice(&1i32.to_be_bytes());
    resp.extend_from_slice(&[0u8; 16]);
    group.bench_function("parse_response", |b| {
        b.iter(|| kafka::parse_kafka_response(black_box(&resp)))
    });

    // Frame completeness
    let mut frame = vec![0, 0, 0, 4];
    frame.extend_from_slice(&[0u8; 4]);
    group.bench_function("frame_complete", |b| {
        b.iter(|| kafka::kafka_frame_complete(black_box(&frame)))
    });

    group.finish();
}

// ─── AMQP ──────────────────────────────────────────────────────────────

fn bench_amqp(c: &mut Criterion) {
    let mut group = c.benchmark_group("amqp");

    // Protocol header
    let header = b"AMQP\x00\x00\x09\x01";
    group.bench_function("parse_header", |b| {
        b.iter(|| amqp::parse_amqp_frame(black_box(header)))
    });

    // Heartbeat
    let heartbeat = [8, 0, 0, 0, 0, 0, 0, 0xCE];
    group.bench_function("parse_heartbeat", |b| {
        b.iter(|| amqp::parse_amqp_frame(black_box(&heartbeat)))
    });

    // Basic.Publish frame
    let mut publish_buf = Vec::new();
    publish_buf.push(1);
    publish_buf.extend_from_slice(&1u16.to_be_bytes());
    let args = vec![0, 0, 4, b't', b'e', b's', b't', 2, b'r', b'k', 0];
    publish_buf.extend_from_slice(&((4 + args.len()) as u32).to_be_bytes());
    publish_buf.extend_from_slice(&60u16.to_be_bytes());
    publish_buf.extend_from_slice(&40u16.to_be_bytes());
    publish_buf.extend_from_slice(&args);
    publish_buf.push(0xCE);
    group.bench_function("parse_basic_publish", |b| {
        b.iter(|| amqp::parse_amqp_frame(black_box(&publish_buf)))
    });

    // Frame length extraction
    group.bench_function("frame_len", |b| {
        b.iter(|| amqp::frame_len(black_box(&publish_buf)))
    });

    // Async method check
    group.bench_function("is_async_method", |b| {
        b.iter(|| amqp::is_async_method(black_box(60), black_box(40)))
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_resp,
    bench_mysql,
    bench_postgres,
    bench_mongodb,
    bench_http,
    bench_memcached,
    bench_kafka,
    bench_amqp,
);

criterion_main!(benches);
