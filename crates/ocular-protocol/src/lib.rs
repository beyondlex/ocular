mod resp;

pub use resp::{RespValue, parse_resp};

/// 一条被解析出的中间件事件
#[derive(Debug, Clone)]
pub struct ProxyEvent {
    pub timestamp: std::time::SystemTime,
    pub component: String,
    pub direction: Direction,
    pub summary: String,
    pub raw: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Request,
    Response,
}
