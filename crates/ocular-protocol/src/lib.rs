mod resp;

pub use resp::{RespValue, parse_resp};

use std::time::{Duration, SystemTime};

/// 一条被解析出的中间件事件
#[derive(Debug, Clone)]
pub struct ProxyEvent {
    pub timestamp: SystemTime,
    pub component: String,
    pub direction: Direction,
    pub summary: String,
    pub raw: Vec<u8>,
    /// response 事件附带的耗时（从最近一次 request 到此 response）
    pub latency: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Request,
    Response,
}
