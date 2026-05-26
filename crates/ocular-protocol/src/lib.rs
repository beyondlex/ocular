pub mod resp;
pub mod mysql;
pub mod amqp;
pub mod postgres;
pub mod mongodb;
pub mod http;
pub mod memcached;
pub mod kafka;
pub mod handler;
pub mod handlers;

pub use resp::{RespValue, parse_resp};
pub use mysql::{parse_mysql_request, parse_mysql_response};
pub use amqp::{parse_amqp_request, parse_amqp_response, format_amqp_response_detail, parse_amqp_frame, parse_amqp_request_full, is_async_method, frame_len as amqp_frame_len};
pub use handler::ProtocolHandler;
pub use handlers::*;

use std::time::{Duration, SystemTime};

/// A single request→response event (merged)
#[derive(Debug, Clone)]
pub struct ProxyEvent {
    pub timestamp: SystemTime,
    pub component: String,
    pub protocol: Protocol,
    /// The command/SQL sent (request summary, truncated for display)
    pub command: String,
    /// Full command extracted from raw request (no truncation)
    pub full_command: String,
    /// Response summary (e.g. "OK", "ResultSet (19 rows, ...)")
    pub response: String,
    /// Formatted response detail for the detail panel
    pub response_detail: String,
    /// Request→response latency
    pub latency: Duration,
    /// Process that initiated the connection (PID + name)
    pub process: Option<String>,
    /// Client address (source)
    pub src: Option<String>,
    /// Remote address (destination)
    pub dest: Option<String>,
    /// Whether this is a system event (error/warning surfaced to TUI)
    pub system: bool,
}

impl ProxyEvent {
    /// Create a system event (error/warning) for display in the events panel
    pub fn system_event(component: &str, message: String) -> Self {
        Self {
            timestamp: SystemTime::now(),
            component: component.to_string(),
            protocol: Protocol::Redis, // unused for system events
            command: message.clone(),
            full_command: message.clone(),
            response: String::new(),
            response_detail: message,
            latency: Duration::ZERO,
            process: None,
            src: None,
            dest: None,
            system: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Request,
    Response,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Redis,
    Mysql,
    Amqp,
    Postgres,
    Mongodb,
    Http,
    Memcached,
    Kafka,
}

impl Protocol {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "redis" => Some(Protocol::Redis),
            "mysql" => Some(Protocol::Mysql),
            "amqp" | "rabbitmq" => Some(Protocol::Amqp),
            "postgres" | "postgresql" => Some(Protocol::Postgres),
            "mongodb" | "mongo" => Some(Protocol::Mongodb),
            "http" | "elasticsearch" | "es" => Some(Protocol::Http),
            "memcached" | "memcache" => Some(Protocol::Memcached),
            "kafka" => Some(Protocol::Kafka),
            _ => None,
        }
    }
}

/// Parse request bytes, returning a human-readable summary (truncated)
pub fn parse_request(protocol: Protocol, buf: &[u8]) -> Option<String> {
    get_handler(protocol).parse_request(buf)
}

/// Extract the full command/SQL from raw bytes (no truncation)
pub fn extract_full_command(protocol: Protocol, buf: &[u8]) -> Option<String> {
    get_handler(protocol).extract_full_command(buf)
}

/// Parse response bytes, returning a short summary
pub fn parse_response(protocol: Protocol, buf: &[u8]) -> Option<String> {
    get_handler(protocol).parse_response(buf)
}

/// Parse response bytes into a detailed display string (for detail panel)
pub fn format_response_detail(protocol: Protocol, buf: &[u8]) -> Option<String> {
    get_handler(protocol).format_response_detail(buf)
}

/// Get the protocol handler for a given protocol.
pub fn get_handler(protocol: Protocol) -> &'static dyn ProtocolHandler {
    match protocol {
        Protocol::Redis => &RedisHandler,
        Protocol::Mysql => &MysqlHandler,
        Protocol::Amqp => &AmqpHandler,
        Protocol::Postgres => &PostgresHandler,
        Protocol::Mongodb => &MongodbHandler,
        Protocol::Http => &HttpHandler,
        Protocol::Memcached => &MemcachedHandler,
        Protocol::Kafka => &KafkaHandler,
    }
}
