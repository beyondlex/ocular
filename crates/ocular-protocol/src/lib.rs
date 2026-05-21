pub mod resp;
pub mod mysql;

pub use resp::{RespValue, parse_resp};
pub use mysql::{parse_mysql_request, parse_mysql_response};

use std::time::{Duration, SystemTime};

/// A parsed middleware event
#[derive(Debug, Clone)]
pub struct ProxyEvent {
    pub timestamp: SystemTime,
    pub component: String,
    pub direction: Direction,
    pub summary: String,
    pub raw: Vec<u8>,
    /// Latency from request to response (only present on response events)
    pub latency: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Request,
    Response,
}

/// Supported protocol types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Redis,
    Mysql,
}

impl Protocol {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "redis" => Some(Protocol::Redis),
            "mysql" => Some(Protocol::Mysql),
            _ => None,
        }
    }
}

/// Parse request bytes, returning a human-readable summary
pub fn parse_request(protocol: Protocol, buf: &[u8]) -> Option<String> {
    match protocol {
        Protocol::Redis => {
            parse_resp(buf).ok().flatten().map(|(val, _)| val.to_command_string())
        }
        Protocol::Mysql => {
            parse_mysql_request(buf).map(|pkt| pkt.to_summary())
        }
    }
}

/// Parse response bytes, returning a human-readable summary
pub fn parse_response(protocol: Protocol, buf: &[u8]) -> Option<String> {
    match protocol {
        Protocol::Redis => {
            parse_resp(buf).ok().flatten().map(|(val, _)| val.to_command_string())
        }
        Protocol::Mysql => {
            parse_mysql_response(buf).map(|r| r.to_summary())
        }
    }
}
