use std::net::IpAddr;

use crate::PendingRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnKey {
    pub src_ip: IpAddr,
    pub src_port: u16,
    pub dst_ip: IpAddr,
    pub dst_port: u16,
}

impl ConnKey {
    pub fn new(src_ip: IpAddr, src_port: u16, dst_ip: IpAddr, dst_port: u16) -> Self {
        Self { src_ip, src_port, dst_ip, dst_port }
    }

    pub fn reverse(&self) -> Self {
        Self {
            src_ip: self.dst_ip,
            src_port: self.dst_port,
            dst_ip: self.src_ip,
            dst_port: self.src_port,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Request,
    Response,
}

pub struct TcpStreamState {
    pub request_buf: Vec<u8>,
    pub response_buf: Vec<u8>,
    pub pending_request: Option<PendingRequest>,
    /// MySQL: tracks whether the handshake phase is complete.
    pub handshake_done: bool,
}

impl TcpStreamState {
    pub fn new() -> Self {
        Self {
            request_buf: Vec::with_capacity(4096),
            response_buf: Vec::with_capacity(4096),
            pending_request: None,
            handshake_done: true, // default true for non-MySQL protocols
        }
    }

    pub fn push_request(&mut self, data: &[u8]) {
        self.request_buf.extend_from_slice(data);
    }

    pub fn push_response(&mut self, data: &[u8]) {
        self.response_buf.extend_from_slice(data);
    }
}
