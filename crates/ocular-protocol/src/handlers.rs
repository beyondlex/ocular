use crate::handler::ProtocolHandler;
use crate::ProxyEvent;

// ─── Redis ──────────────────────────────────────────────────────────────────

pub struct RedisHandler;

impl ProtocolHandler for RedisHandler {
    fn parse_request(&self, buf: &[u8]) -> Option<String> {
        crate::parse_resp(buf).ok().flatten().map(|(val, _)| val.to_command_string())
    }
    fn parse_response(&self, buf: &[u8]) -> Option<String> {
        crate::parse_resp(buf).ok().flatten().map(|(val, _)| val.to_command_string())
    }
}

// ─── MySQL ──────────────────────────────────────────────────────────────────

pub struct MysqlHandler;

impl ProtocolHandler for MysqlHandler {
    fn parse_request(&self, buf: &[u8]) -> Option<String> {
        crate::parse_mysql_request(buf).map(|pkt| pkt.to_summary())
    }
    fn extract_full_command(&self, buf: &[u8]) -> Option<String> {
        if buf.len() < 5 { return None; }
        let payload_len = (buf[0] as usize) | (buf[1] as usize) << 8 | (buf[2] as usize) << 16;
        if buf.len() < 4 + payload_len || payload_len <= 1 { return None; }
        let cmd = buf[4];
        if cmd == 0x03 || cmd == 0x16 {
            let sql = String::from_utf8_lossy(&buf[5..4 + payload_len]);
            Some(sql.replace(|c: char| c.is_control(), ""))
        } else {
            self.parse_request(buf)
        }
    }
    fn parse_response(&self, buf: &[u8]) -> Option<String> {
        crate::parse_mysql_response(buf).map(|r| r.to_summary())
    }
    fn format_response_detail(&self, buf: &[u8]) -> Option<String> {
        crate::parse_mysql_response(buf).map(|r| r.to_display())
    }
    fn needs_response_buffering(&self) -> bool { true }
    fn response_complete(&self, buf: &[u8]) -> bool {
        crate::mysql::mysql_response_complete(buf)
    }
}

// ─── PostgreSQL ─────────────────────────────────────────────────────────────

pub struct PostgresHandler;

impl ProtocolHandler for PostgresHandler {
    fn parse_request(&self, buf: &[u8]) -> Option<String> {
        crate::postgres::parse_postgres_request(buf)
    }
    fn extract_full_command(&self, buf: &[u8]) -> Option<String> {
        crate::postgres::extract_postgres_full_command(buf)
    }
    fn parse_response(&self, buf: &[u8]) -> Option<String> {
        crate::postgres::parse_postgres_response(buf)
    }
    fn format_response_detail(&self, buf: &[u8]) -> Option<String> {
        crate::postgres::format_postgres_response_detail(buf)
    }
    fn needs_response_buffering(&self) -> bool { true }
    fn response_complete(&self, buf: &[u8]) -> bool {
        crate::postgres::postgres_response_complete(buf)
    }
}

// ─── AMQP ───────────────────────────────────────────────────────────────────

pub struct AmqpHandler;

impl ProtocolHandler for AmqpHandler {
    fn parse_request(&self, buf: &[u8]) -> Option<String> {
        crate::amqp::parse_amqp_request(buf)
    }
    fn parse_response(&self, buf: &[u8]) -> Option<String> {
        crate::amqp::parse_amqp_response(buf)
    }
    fn format_response_detail(&self, buf: &[u8]) -> Option<String> {
        crate::amqp::format_amqp_response_detail(buf)
    }
    fn is_frame_based(&self) -> bool { true }
}

// ─── MongoDB ────────────────────────────────────────────────────────────────

pub struct MongodbHandler;

impl ProtocolHandler for MongodbHandler {
    fn parse_request(&self, buf: &[u8]) -> Option<String> {
        crate::mongodb::parse_mongo_request(buf)
    }
    fn extract_full_command(&self, buf: &[u8]) -> Option<String> {
        crate::mongodb::extract_mongo_full_command(buf)
    }
    fn parse_response(&self, buf: &[u8]) -> Option<String> {
        crate::mongodb::parse_mongo_response(buf)
    }
    fn format_response_detail(&self, buf: &[u8]) -> Option<String> {
        crate::mongodb::format_mongo_response_detail(buf)
    }
}

// ─── HTTP ───────────────────────────────────────────────────────────────────

pub struct HttpHandler;


impl ProtocolHandler for HttpHandler {
    fn parse_request(&self, buf: &[u8]) -> Option<String> {
        crate::http::parse_http_request(buf)
    }
    fn extract_full_command(&self, buf: &[u8]) -> Option<String> {
        crate::http::extract_http_full_command(buf)
    }
    fn parse_response(&self, buf: &[u8]) -> Option<String> {
        crate::http::parse_http_response(buf)
    }
    fn format_response_detail(&self, buf: &[u8]) -> Option<String> {
        crate::http::format_http_response_detail(buf)
    }
    fn to_replay_command(&self, ev: &ProxyEvent) -> String {
        let dest = ev.dest.as_deref().unwrap_or("localhost");
        let lines: Vec<&str> = ev.full_command.lines().collect();
        let first_line = lines.first().copied().unwrap_or("");
        let mut parts = first_line.splitn(2, ' ');
        let method = parts.next().unwrap_or("GET");
        let path = parts.next().unwrap_or("/");
        let url = format!("http://{}{}", dest, path);
        let mut curl = format!("curl -X {} '{}'", method, url);

        let mut in_headers = false;
        let mut in_body = false;
        let mut body = String::new();
        for line in &lines[1..] {
            if *line == "[Request Headers]" { in_headers = true; in_body = false; continue; }
            if *line == "[Request Body]" { in_body = true; in_headers = false; continue; }
            if line.starts_with('[') && line.ends_with(']') { in_headers = false; in_body = false; continue; }
            if line.is_empty() { continue; }
            if in_headers { curl.push_str(&format!(" \\\n  -H '{}'", line)); }
            if in_body { body.push_str(line); }
        }
        if !body.is_empty() { curl.push_str(&format!(" \\\n  -d '{}'", body)); }
        curl
    }
    fn needs_request_buffering(&self) -> bool { true }
    fn needs_response_buffering(&self) -> bool { true }
    fn request_complete(&self, buf: &[u8]) -> bool {
        crate::http::http_request_complete(buf)
    }
    fn response_complete(&self, buf: &[u8]) -> bool {
        crate::http::http_response_complete(buf)
    }
}

// ─── Memcached ──────────────────────────────────────────────────────────────

pub struct MemcachedHandler;

impl ProtocolHandler for MemcachedHandler {
    fn parse_request(&self, buf: &[u8]) -> Option<String> {
        crate::memcached::parse_memcached_request(buf)
    }
    fn parse_response(&self, buf: &[u8]) -> Option<String> {
        crate::memcached::parse_memcached_response(buf)
    }
    fn format_response_detail(&self, buf: &[u8]) -> Option<String> {
        crate::memcached::format_memcached_response_detail(buf)
    }
    fn needs_request_buffering(&self) -> bool { true }
    fn needs_response_buffering(&self) -> bool { true }
    fn request_complete(&self, buf: &[u8]) -> bool {
        crate::memcached::memcached_request_complete(buf)
    }
    fn response_complete(&self, buf: &[u8]) -> bool {
        crate::memcached::memcached_response_complete(buf)
    }
}

// ─── Kafka ──────────────────────────────────────────────────────────────────

pub struct KafkaHandler;

impl ProtocolHandler for KafkaHandler {
    fn parse_request(&self, buf: &[u8]) -> Option<String> {
        crate::kafka::parse_kafka_request(buf)
    }
    fn extract_full_command(&self, buf: &[u8]) -> Option<String> {
        crate::kafka::extract_kafka_full_command(buf)
    }
    fn parse_response(&self, buf: &[u8]) -> Option<String> {
        crate::kafka::parse_kafka_response(buf)
    }
    fn format_response_detail(&self, buf: &[u8]) -> Option<String> {
        crate::kafka::format_kafka_response_detail(buf)
    }
    fn needs_request_buffering(&self) -> bool { true }
    fn needs_response_buffering(&self) -> bool { true }
    fn request_complete(&self, buf: &[u8]) -> bool {
        crate::kafka::kafka_frame_complete(buf)
    }
    fn response_complete(&self, buf: &[u8]) -> bool {
        crate::kafka::kafka_frame_complete(buf)
    }
}
