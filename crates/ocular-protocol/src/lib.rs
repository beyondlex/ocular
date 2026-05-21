pub mod resp;
pub mod mysql;

pub use resp::{RespValue, parse_resp};
pub use mysql::{parse_mysql_request, parse_mysql_response};

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

/// Parse request bytes, returning a human-readable summary (truncated)
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

/// Extract the full command/SQL from raw bytes (no truncation)
pub fn extract_full_command(protocol: Protocol, buf: &[u8]) -> Option<String> {
    match protocol {
        Protocol::Redis => {
            parse_resp(buf).ok().flatten().map(|(val, _)| val.to_command_string())
        }
        Protocol::Mysql => {
            if buf.len() < 5 { return None; }
            let payload_len = (buf[0] as usize) | (buf[1] as usize) << 8 | (buf[2] as usize) << 16;
            if buf.len() < 4 + payload_len || payload_len <= 1 { return None; }
            let cmd = buf[4];
            if cmd == 0x03 || cmd == 0x16 {
                let sql = String::from_utf8_lossy(&buf[5..4 + payload_len]);
                Some(sql.replace(|c: char| c.is_control(), ""))
            } else {
                parse_mysql_request(buf).map(|pkt| pkt.to_summary())
            }
        }
    }
}

/// Parse response bytes, returning a short summary
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

/// Parse response bytes into a detailed display string (for detail panel)
pub fn format_response_detail(protocol: Protocol, buf: &[u8]) -> Option<String> {
    match protocol {
        Protocol::Redis => {
            parse_resp(buf).ok().flatten().map(|(val, _)| val.to_command_string())
        }
        Protocol::Mysql => {
            parse_mysql_response(buf).map(|r| r.to_display())
        }
    }
}
