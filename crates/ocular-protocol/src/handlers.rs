use crate::handler::{HandshakeAction, ProtocolHandler};
use crate::{Direction, ProxyEvent};

// ─── Redis ──────────────────────────────────────────────────────────────────

pub struct RedisHandler;

impl ProtocolHandler for RedisHandler {
    fn parse_request(&self, buf: &[u8]) -> Option<String> {
        crate::parse_resp(buf).ok().flatten().map(|(val, _)| val.to_command_string())
    }
    fn parse_response(&self, buf: &[u8]) -> Option<String> {
        crate::parse_resp(buf).ok().flatten().map(|(val, _)| val.to_command_string())
    }
    fn default_port(&self) -> u16 { 6379 }
}

// ─── MySQL ──────────────────────────────────────────────────────────────────

const MYSQL_HEADER_LEN: usize = 4; // 3-byte length + 1-byte sequence
const MYSQL_COM_QUERY: u8 = 0x03;
const MYSQL_COM_STMT_PREPARE: u8 = 0x16;
const MYSQL_GREETING_PROTOCOL_VERSION: u8 = 10;
const MYSQL_OK_MARKER: u8 = 0x00;

pub struct MysqlHandler;

impl ProtocolHandler for MysqlHandler {
    fn parse_request(&self, buf: &[u8]) -> Option<String> {
        crate::parse_mysql_request(buf).map(|pkt| pkt.to_summary())
    }
    fn extract_full_command(&self, buf: &[u8]) -> Option<String> {
        if buf.len() < 5 { return None; }
        let payload_len = (buf[0] as usize) | (buf[1] as usize) << 8 | (buf[2] as usize) << 16;
        if buf.len() < MYSQL_HEADER_LEN + payload_len || payload_len <= 1 { return None; }
        let cmd = buf[4];
        if cmd == MYSQL_COM_QUERY || cmd == MYSQL_COM_STMT_PREPARE {
            let sql = String::from_utf8_lossy(&buf[5..MYSQL_HEADER_LEN + payload_len]);
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
    fn message_length(&self, buf: &[u8]) -> Option<usize> {
        if buf.len() < MYSQL_HEADER_LEN { return None; }
        let pkt_len = (buf[0] as usize) | (buf[1] as usize) << 8 | (buf[2] as usize) << 16;
        Some(MYSQL_HEADER_LEN + pkt_len)
    }
    fn capture_handshake(&self, payload: &[u8], direction: Direction) -> HandshakeAction {
        if payload.len() < 5 { return HandshakeAction::Skip; }
        let seq = payload[3];
        let marker = payload[4];
        match direction {
            Direction::Request if seq == 0 => HandshakeAction::Done, // real command
            Direction::Response if seq == 0 && marker == MYSQL_GREETING_PROTOCOL_VERSION => HandshakeAction::Skip,
            Direction::Response if marker == MYSQL_OK_MARKER => HandshakeAction::Complete,
            _ => HandshakeAction::Skip, // auth exchange
        }
    }
    fn default_port(&self) -> u16 { 3306 }
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
    fn default_port(&self) -> u16 { 5432 }
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
    fn default_port(&self) -> u16 { 5672 }
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
    fn message_length(&self, buf: &[u8]) -> Option<usize> {
        if buf.len() < 4 { return None; }
        let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if len > 0 { Some(len) } else { None }
    }
    fn default_port(&self) -> u16 { 27017 }
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
    fn default_port(&self) -> u16 { 9200 }
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
    fn default_port(&self) -> u16 { 11211 }
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
    fn default_port(&self) -> u16 { 9092 }
}
