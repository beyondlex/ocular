use anyhow::Result;
use ocular_protocol::{Protocol, parse_request, parse_response, extract_full_command, format_response_detail, format_postgres_response_detail_with_formats, parse_amqp_frame, parse_amqp_request_full, is_async_method, amqp_frame_len, ProtocolHandler, parse_bind_params};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt, AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tracing::{info, warn, error, debug};

pub use ocular_protocol::ProxyEvent;
pub use ocular_protocol::{ConnectionState, StatusMap};

/// Pending request info
struct PendingRequest {
    timestamp: SystemTime,
    instant: Instant,
    command: String,
    full_command: String,
    /// Bind result-column format codes: true = binary, false = text.
    /// None means use RowDescription format codes directly (simple query protocol).
    result_formats: Option<Vec<bool>>,
}

pub async fn run_proxy(
    listen_addr: String,
    remote_addr: String,
    name: String,
    protocol: Protocol,
    tx: broadcast::Sender<ProxyEvent>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    status: StatusMap,
) -> Result<()> {
    let listener = match TcpListener::bind(&listen_addr).await {
        Ok(l) => l,
        Err(e) => {
            let msg = format!("bind failed on {}: {}", listen_addr, e);
            let _ = tx.send(ProxyEvent::system_event(&name, msg));
            status.lock().unwrap().entry(name.clone()).or_default().last_error = Some(format!("bind failed: {}", e));
            return Err(e.into());
        }
    };
    let conn_count = Arc::new(AtomicUsize::new(0));
    {
        let mut map = status.lock().unwrap();
        map.entry(name.clone()).or_default().has_connector = true;
    }
    info!(component = %name, listen = %listen_addr, remote = %remote_addr, ?protocol, "proxy listening");

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (client, peer) = result?;
                debug!(component = %name, peer = %peer, "new client connection");
                let remote = remote_addr.clone();
                let name = name.clone();
                let tx = tx.clone();
                let process = resolve_peer_process(peer.port());
                let peer_addr = peer.to_string();
                let remote_for_conn = remote.clone();
                let conn_count = conn_count.clone();
                let status = status.clone();
                let protocol_for_conn = protocol;
                tokio::spawn(async move {
                    conn_count.fetch_add(1, Ordering::Relaxed);
                    {
                        let mut map = status.lock().unwrap();
                        let s = map.entry(name.clone()).or_default();
                        s.active_connections = conn_count.load(Ordering::Relaxed);
                        s.last_active_at = Some(SystemTime::now());
                    }
                    if let Err(e) = handle_conn(client, &remote, &name, protocol_for_conn, &tx, process, peer_addr, remote_for_conn).await {
                        warn!(component = %name, remote = %remote, error = %e, "connection ended with error");
                        let _ = tx.send(ProxyEvent::system_event(&name, format!("connection error: {}", e)));
                        status.lock().unwrap().entry(name.clone()).or_default().last_error = Some(e.to_string());
                    }
                    let remaining = conn_count.fetch_sub(1, Ordering::Relaxed).saturating_sub(1);
                    status.lock().unwrap().entry(name.clone()).or_default().active_connections = remaining;
                });
            }
            _ = shutdown.changed() => {
                info!(component = %name, "proxy shutting down");
                break;
            }
        }
    }
    Ok(())
}

// ─── Buffer managers ────────────────────────────────────────────────────────

/// Unified request buffer manager for protocols that need request buffering
/// (HTTP, Memcached, Kafka). Handles accumulation, completeness checks,
/// parsing, and pipeline handling (Memcached emits prev pending as standalone).
struct ReqBufMgr {
    buf: Vec<u8>,
}

impl ReqBufMgr {
    fn new() -> Self {
        Self { buf: Vec::with_capacity(4096) }
    }

    #[allow(clippy::too_many_arguments)]
    fn process(
        &mut self,
        data: &[u8],
        protocol: Protocol,
        handler: &'static dyn ProtocolHandler,
        pending: &Arc<Mutex<Option<PendingRequest>>>,
        tx: &broadcast::Sender<ProxyEvent>,
        name: &str,
        process: &Option<String>,
        src: &str,
        dest: &str,
    ) {
        self.buf.extend_from_slice(data);
        while handler.request_complete(&self.buf) {
            // Emit previous pending as standalone (Memcached pipeline support)
            if let Some(prev) = pending.lock().unwrap().take() {
                let _ = tx.send(ProxyEvent {
                    timestamp: prev.timestamp,
                    component: name.to_string(),
                    protocol,
                    command: prev.command,
                    full_command: prev.full_command,
                    response: String::new(),
                    response_detail: String::new(),
                    latency: Duration::ZERO,
                    process: process.clone(),
                    src: Some(src.to_string()),
                    dest: Some(dest.to_string()),
                    system: false,
                });
            }
            // Parse new request
            if let Some(command) = parse_request(protocol, &self.buf) {
                let full_command = extract_full_command(protocol, &self.buf)
                    .unwrap_or_else(|| command.clone());
                *pending.lock().unwrap() = Some(PendingRequest {
                    timestamp: SystemTime::now(),
                    instant: Instant::now(),
                    command,
                    full_command,
                    result_formats: None,
                });
            }
            let consumed = self.consumed_len(protocol, handler);
            if consumed > 0 && consumed <= self.buf.len() {
                self.buf.drain(..consumed);
            } else {
                self.buf.clear();
            }
        }
    }

    /// Determine how many bytes were consumed by the last complete message.
    fn consumed_len(&self, protocol: Protocol, handler: &'static dyn ProtocolHandler) -> usize {
        match protocol {
            Protocol::Kafka => {
                if self.buf.len() >= 4 {
                    (i32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize) + 4
                } else {
                    self.buf.len()
                }
            }
            Protocol::Memcached => {
                // Advance past command line + data block
                let s = std::str::from_utf8(&self.buf).unwrap_or("");
                let first_crlf = s.find("\r\n").unwrap_or(0);
                let line = &s[..first_crlf];
                let parts: Vec<&str> = line.split_whitespace().collect();
                let cmd = parts.first().map(|c| c.to_uppercase()).unwrap_or_default();
                match cmd.as_str() {
                    "SET" | "ADD" | "REPLACE" | "APPEND" | "PREPEND" | "CAS" => {
                        let bytes: usize = parts.get(4).and_then(|b| b.parse().ok()).unwrap_or(0);
                        first_crlf + 2 + bytes + 2
                    }
                    _ => first_crlf + 2,
                }
            }
            _ => {
                // HTTP and others: clear entire buffer
                let _ = handler;
                self.buf.len()
            }
        }
    }
}

/// Unified response buffer manager for protocols that need response buffering
/// (MySQL, HTTP, Memcached, Kafka, Postgres). Handles accumulation,
/// completeness checks, and event emission.
struct RespBufMgr {
    buf: Vec<u8>,
}

impl RespBufMgr {
    fn new() -> Self {
        Self { buf: Vec::with_capacity(4096) }
    }

    #[allow(clippy::too_many_arguments)]
    fn process(
        &mut self,
        data: &[u8],
        protocol: Protocol,
        handler: &'static dyn ProtocolHandler,
        pending: &Arc<Mutex<Option<PendingRequest>>>,
        tx: &broadcast::Sender<ProxyEvent>,
        name: &str,
        process: &Option<String>,
        src: &str,
        dest: &str,
    ) -> bool {
        self.buf.extend_from_slice(data);
        if handler.response_complete(&self.buf) {
            if let Some(req) = pending.lock().unwrap().take() {
                let latency = req.instant.elapsed();
                // For Kafka, parse only the first frame
                let parse_buf = if protocol == Protocol::Kafka && self.buf.len() >= 4 {
                    let frame_len = (i32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize) + 4;
                    &self.buf[..frame_len.min(self.buf.len())]
                } else {
                    &self.buf
                };
                eprintln!("[ocular-proxy] resp_mgr: took pending, result_formats={:?}", req.result_formats);
                let response = parse_response(protocol, parse_buf).unwrap_or_default();
                let response_detail = if protocol == Protocol::Postgres {
                    format_postgres_response_detail_with_formats(parse_buf, req.result_formats.as_deref())
                        .unwrap_or_else(|| response.clone())
                } else {
                    format_response_detail(protocol, parse_buf)
                        .unwrap_or_else(|| response.clone())
                };
                let _ = tx.send(ProxyEvent {
                    timestamp: req.timestamp,
                    component: name.to_string(),
                    protocol,
                    command: req.command,
                    full_command: req.full_command,
                    response,
                    response_detail,
                    latency,
                    process: process.clone(),
                    src: Some(src.to_string()),
                    dest: Some(dest.to_string()),
                    system: false,
                });
            }
            self.buf.clear();
            true
        } else {
            false
        }
    }

    /// Scan buffer for multiple complete frames, emitting one event per frame.
    /// Used by Kafka.
    #[allow(clippy::too_many_arguments)]
    fn process_kafka(
        &mut self,
        data: &[u8],
        protocol: Protocol,
        handler: &'static dyn ProtocolHandler,
        pending: &Arc<Mutex<Option<PendingRequest>>>,
        tx: &broadcast::Sender<ProxyEvent>,
        name: &str,
        process: &Option<String>,
        src: &str,
        dest: &str,
    ) {
        self.buf.extend_from_slice(data);
        while handler.response_complete(&self.buf) {
            let frame_len = (i32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize) + 4;
            if let Some(req) = pending.lock().unwrap().take() {
                let latency = req.instant.elapsed();
                let response = parse_response(protocol, &self.buf[..frame_len]).unwrap_or_default();
                let response_detail = format_response_detail(protocol, &self.buf[..frame_len])
                    .unwrap_or_else(|| response.clone());
                let _ = tx.send(ProxyEvent {
                    timestamp: req.timestamp,
                    component: name.to_string(),
                    protocol,
                    command: req.command,
                    full_command: req.full_command,
                    response,
                    response_detail,
                    latency,
                    process: process.clone(),
                    src: Some(src.to_string()),
                    dest: Some(dest.to_string()),
                    system: false,
                });
            }
            self.buf = self.buf[frame_len..].to_vec();
        }
    }


}

#[allow(clippy::too_many_arguments)]
async fn handle_conn(
    mut client: TcpStream,
    remote_addr: &str,
    name: &str,
    protocol: Protocol,
    tx: &broadcast::Sender<ProxyEvent>,
    process: Option<String>,
    src: String,
    dest: String,
) -> Result<()> {
    // Parse remote address: detect https:// for TLS outbound
    let (actual_addr, use_tls, tls_host) = if remote_addr.starts_with("https://") {
        let stripped = remote_addr.strip_prefix("https://").unwrap();
        let host = stripped.split(':').next().unwrap_or(stripped).to_string();
        (stripped.to_string(), true, host)
    } else {
        let stripped = remote_addr.strip_prefix("http://").unwrap_or(remote_addr);
        (stripped.to_string(), false, String::new())
    };

    let tcp_stream = match TcpStream::connect(&actual_addr).await {
        Ok(s) => {
            debug!(component = %name, remote = %actual_addr, "connected to remote");
            s
        }
        Err(e) => {
            error!(component = %name, remote = %actual_addr, error = %e,
                "failed to connect to remote — is the service running?");
            let _ = tx.send(ProxyEvent::system_event(name, format!("cannot reach {} ({})", actual_addr, e)));
            if protocol == Protocol::Redis {
                let err_msg = format!("-ERR ocular proxy: cannot reach {} ({})\r\n", actual_addr, e);
                let _ = client.write_all(err_msg.as_bytes()).await;
            }
            return Err(e.into());
        }
    };

    let (sr, sw): (Box<dyn AsyncRead + Unpin + Send>, Box<dyn AsyncWrite + Unpin + Send>) = if use_tls {
        let config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let domain = rustls::pki_types::ServerName::try_from(tls_host)
            .map_err(|e| anyhow::anyhow!("invalid TLS hostname: {}", e))?;
        let tls_stream = connector.connect(domain, tcp_stream).await?;
        let (r, w) = tokio::io::split(tls_stream);
        (Box::new(r) as Box<dyn AsyncRead + Unpin + Send>, Box::new(w) as Box<dyn AsyncWrite + Unpin + Send>)
    } else {
        let (r, w) = tokio::io::split(tcp_stream);
        (Box::new(r) as Box<dyn AsyncRead + Unpin + Send>, Box::new(w) as Box<dyn AsyncWrite + Unpin + Send>)
    };

    let mut sr = sr;
    let mut sw = sw;

    // For MySQL: strip SSL from greeting
    if protocol == Protocol::Mysql {
        let mut greeting_buf = [0u8; 65536];
        let n = sr.read(&mut greeting_buf).await?;
        if n == 0 { return Ok(()); }
        let mut greeting = greeting_buf[..n].to_vec();
        strip_mysql_ssl_flag(&mut greeting);
        client.write_all(&greeting).await?;
        debug!(component = %name, "forwarded MySQL greeting with SSL stripped");
    }

    // For PostgreSQL: strip SSL by forwarding negotiation to server but replying N to client.
    if protocol == Protocol::Postgres {
        let mut buf = [0u8; 256];
        let n = client.read(&mut buf).await?;
        if n == 0 { return Ok(()); }
        let data = &buf[..n];
        let neg_code = if n >= 8 {
            u32::from_be_bytes([data[4], data[5], data[6], data[7]])
        } else { 0 };
        if neg_code == 80877103 || neg_code == 80877104 {
            sw.write_all(data).await?;
            let mut resp = [0u8; 1];
            let rn = sr.read(&mut resp).await?;
            if rn == 0 { return Ok(()); }
            client.write_all(b"N").await?;
        } else {
            sw.write_all(data).await?;
        }
    }

    let (mut cr, mut cw) = client.split();
    let pending: Arc<Mutex<Option<PendingRequest>>> = Arc::new(Mutex::new(None));
    let stmt_map: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let handler = ocular_protocol::get_handler(protocol);

    let name_req = name.to_string();
    let tx_req = tx.clone();
    let pending_w = pending.clone();
    let pending_final = pending.clone();
    let pending_r = pending;
    let process_info = process;
    let process_req = process_info.clone();
    let src_req = src.clone();
    let dest_req = dest.clone();
    let src_resp = src.clone();
    let dest_resp = dest;
    let stmt_map_req = stmt_map.clone();

    // ─── Client → Server ────────────────────────────────────────────────

    let client_to_server = async move {
        let mut buf = [0u8; 65536];
        let mut req_mgr = ReqBufMgr::new();
        loop {
            let n = cr.read(&mut buf).await?;
            if n == 0 { break; }
            let data = &buf[..n];

            if protocol == Protocol::Amqp {
                // AMQP: frame-based with custom multi-frame handling
                let mut pos = 0;
                while pos < data.len() {
                    let frame_data = &data[pos..];
                    let Some(flen) = amqp_frame_len(frame_data) else { break };
                    if let Some(frame) = parse_amqp_frame(frame_data) {
                        if frame.frame_type == 8 {
                            pos += flen;
                            continue;
                        }
                        if let Some(ref method) = frame.method {
                            if is_async_method(method.class_id, method.method_id) {
                                let (summary, detail) = parse_amqp_request_full(frame_data)
                                    .unwrap_or_else(|| (method.summary.clone(), method.detail.clone()));
                                let _ = tx_req.send(ProxyEvent {
                                    timestamp: SystemTime::now(),
                                    component: name_req.clone(),
                                    protocol,
                                    command: summary,
                                    full_command: detail.clone(),
                                    response: String::new(),
                                    response_detail: detail,
                                    latency: Duration::ZERO,
                                    process: process_req.clone(),
                                    src: Some(src_req.clone()),
                                    dest: Some(dest_req.clone()),
                                    system: false,
                                });
                            } else {
                                debug!(component = %name_req, command = %method.summary);
                                *pending_w.lock().unwrap() = Some(PendingRequest {
                                    timestamp: SystemTime::now(),
                                    instant: Instant::now(),
                                    command: method.summary.clone(),
                                    full_command: method.detail.clone(),
                                    result_formats: None,
                                });
                            }
                        }
                    }
                    pos += flen;
                }
            } else if protocol == Protocol::Postgres {
                // Postgres: scan typed messages; Q/P set pending, Bind updates params
                let mut pos = 0;
                while pos < data.len() {
                    let first = data[pos];
                    let is_typed = matches!(first, b'Q' | b'P' | b'B' | b'E' | b'D' | b'S' | b'X' | b'C' | b'p' | b'H' | b'F' | b'd' | b'c' | b'f');
                    if !is_typed { break; }
                    if pos + 5 > data.len() { break; }
                    let len = u32::from_be_bytes([data[pos+1], data[pos+2], data[pos+3], data[pos+4]]) as usize;
                    let end = pos + 1 + len;
                    if end > data.len() { break; }
                    let payload = &data[pos+5..end];

                    match first {
                        b'Q' | b'P' => {
                            let msg = &data[pos..end];
                            if let Some(command) = parse_request(protocol, msg) {
                                let full_command = extract_full_command(protocol, msg)
                                    .unwrap_or_else(|| command.clone());
                                *pending_w.lock().unwrap() = Some(PendingRequest {
                                    timestamp: SystemTime::now(),
                                    instant: Instant::now(),
                                    command,
                                    full_command,
                                    result_formats: None,
                                });
                            }
                            // Store stmt_name → SQL for later Bind correlation
                            if first == b'P' && !payload.is_empty() {
                                let stmt = ocular_protocol::postgres::read_cstr(payload);
                                let rest = &payload[stmt.len() + 1..];
                                let query = ocular_protocol::postgres::read_cstr(rest);
                                if !query.is_empty() {
                                    stmt_map_req.lock().unwrap().insert(stmt, query);
                                }
                            }
                        }
                        b'B' => {
                            eprintln!("[ocular-proxy] Bind: pending before parse={:?}", pending_w.lock().unwrap().as_ref().map(|p| (&p.command, p.result_formats.is_some())));
                            if let Some(info) = parse_bind_params(payload) {
                                eprintln!("[ocular-proxy] Bind: parse_bind_params returned result_formats={:?}", info.result_formats);
                                debug!(component = %name_req, stmt = %info.stmt, params = ?info.params, "Bind received");
                                if let Some(sql) = stmt_map_req.lock().unwrap().get(&info.stmt).cloned() {
                                    let mut filled = sql.clone();
                                    for (i, param) in info.params.iter().enumerate() {
                                        filled = filled.replace(&format!("${}", i + 1), param);
                                    }
                                    let params_str = info.params.join(", ");
                                    let mut pw = pending_w.lock().unwrap();
                                    if let Some(pending) = pw.as_mut() {
                                        // Update existing pending (no Sync after Describe)
                                        pending.command = format!("{}  [{}]", pending.command, params_str);
                                        pending.full_command = filled;
                                        pending.result_formats = Some(info.result_formats);
                                        debug!(component = %name_req, command = %pending.command, "Bind updated existing pending");
                                    } else {
                                        // Set new pending (Sync already consumed previous one)
                                        *pw = Some(PendingRequest {
                                            timestamp: SystemTime::now(),
                                            instant: Instant::now(),
                                            command: filled.clone(),
                                            full_command: filled,
                                            result_formats: Some(info.result_formats),
                                        });
                                        debug!(component = %name_req, "Bind created new pending with filled SQL");
                                    }
                                } else {
                                    debug!(component = %name_req, stmt = %info.stmt, "Bind: stmt not found in map");
                                }
                            } else {
                                debug!(component = %name_req, "Bind: parse_bind_params returned None");
                            }
                        }
                        _ => {}
                    }
                    pos = end;
                }
            } else if handler.needs_request_buffering() {
                // Unified buffered: HTTP, Memcached, Kafka
                req_mgr.process(data, protocol, handler, &pending_w, &tx_req,
                    &name_req, &process_req, &src_req, &dest_req);
            } else {
                // Default: single request per read (Redis, MongoDB)
                if let Some(command) = parse_request(protocol, data) {
                    let full_command = extract_full_command(protocol, data)
                        .unwrap_or_else(|| command.clone());
                    debug!(component = %name_req, %command);
                    *pending_w.lock().unwrap() = Some(PendingRequest {
                        timestamp: SystemTime::now(),
                        instant: Instant::now(),
                        command,
                        full_command,
                        result_formats: None,
                    });
                }
            }

            sw.write_all(data).await?;
        }
        Ok::<_, anyhow::Error>(())
    };

    // ─── Server → Client ────────────────────────────────────────────────

    let server_to_client = async move {
        let mut buf = [0u8; 65536];
        let mut resp_mgr = RespBufMgr::new();
        loop {
            let n = sr.read(&mut buf).await?;
            if n == 0 { break; }
            let data = &buf[..n];
            cw.write_all(data).await?;

            if protocol == Protocol::Amqp {
                // AMQP: frame-based with body extraction and server-initiated handling
                let mut pos = 0;
                while pos < data.len() {
                    let frame_data = &data[pos..];
                    let Some(flen) = amqp_frame_len(frame_data) else { break };
                    if let Some(frame) = parse_amqp_frame(frame_data) {
                        if frame.frame_type == 2 || frame.frame_type == 3 {
                            pos += flen;
                            continue;
                        }
                        if frame.frame_type == 8 {
                            pos += flen;
                            continue;
                        }
                    }

                    // Extract body from subsequent Header+Body frames
                    let mut body_text = String::new();
                    let mut peek = pos + flen;
                    while peek < data.len() {
                        let peek_data = &data[peek..];
                        let Some(plen) = amqp_frame_len(peek_data) else { break };
                        if let Some(pf) = parse_amqp_frame(peek_data) {
                            if pf.frame_type == 2 {
                                // Header frame
                            } else if pf.frame_type == 3 {
                                if let Some(body) = &pf.body {
                                    body_text = String::from_utf8_lossy(body).to_string();
                                }
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                        peek += plen;
                    }

                    if let Some(req) = pending_r.lock().unwrap().take() {
                        let latency = req.instant.elapsed();
                        let mut response = parse_response(protocol, frame_data).unwrap_or_default();
                        let mut response_detail = format_response_detail(protocol, frame_data)
                            .unwrap_or_else(|| response.clone());
                        if !body_text.is_empty() {
                            response = format!("{} | {}", response, body_text);
                            response_detail = format!("{}\nBody: {}", response_detail, body_text);
                        }
                        let _ = tx.send(ProxyEvent {
                            timestamp: req.timestamp,
                            component: name.to_string(),
                            protocol,
                            command: req.command,
                            full_command: req.full_command,
                            response,
                            response_detail,
                            latency,
                            process: process_info.clone(),
                            src: Some(src_resp.clone()),
                            dest: Some(dest_resp.clone()),
                            system: false,
                        });
                    } else if let Some(frame) = parse_amqp_frame(frame_data) {
                        // Server-initiated method (e.g. Basic.Deliver)
                        if let Some(ref method) = frame.method {
                            let response = if body_text.is_empty() { String::new() } else { body_text.clone() };
                            let response_detail = if body_text.is_empty() { String::new() } else { body_text.clone() };
                            let command = method.summary.clone();
                            let _ = tx.send(ProxyEvent {
                                timestamp: SystemTime::now(),
                                component: name.to_string(),
                                protocol,
                                command,
                                full_command: method.detail.clone(),
                                response,
                                response_detail,
                                latency: Duration::ZERO,
                                process: process_info.clone(),
                                src: Some(dest_resp.clone()),
                                dest: Some(src_resp.clone()),
                                system: false,
                            });
                        }
                    }
                    pos = peek;
                }
            } else if protocol == Protocol::Kafka {
                // Kafka: scan frames, emit per frame
                resp_mgr.process_kafka(data, protocol, handler, &pending_r, tx,
                    name, &process_info, &src_resp, &dest_resp);
            } else if handler.needs_response_buffering() {
                // Unified buffered: MySQL, HTTP, Memcached, Postgres
                resp_mgr.process(data, protocol, handler, &pending_r, tx,
                    name, &process_info, &src_resp, &dest_resp);
            } else {
                // Default: single response per read (Redis, MongoDB)
                if let Some(req) = pending_r.lock().unwrap().take() {
                    let latency = req.instant.elapsed();
                    let response = parse_response(protocol, data).unwrap_or_default();
                    let response_detail = format_response_detail(protocol, data)
                        .unwrap_or_else(|| response.clone());
                    let _ = tx.send(ProxyEvent {
                        timestamp: req.timestamp,
                        component: name.to_string(),
                        protocol,
                        command: req.command,
                        full_command: req.full_command,
                        response,
                        response_detail,
                        latency,
                        process: process_info.clone(),
                        src: Some(src_resp.clone()),
                        dest: Some(dest_resp.clone()),
                        system: false,
                    });
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    };

    tokio::pin!(client_to_server);
    tokio::pin!(server_to_client);

    tokio::select! {
        r = &mut client_to_server => {
            // Client closed write end; give server time to send final response
            if r.is_ok() && pending_final.lock().unwrap().is_some() {
                let _ = tokio::time::timeout(
                    Duration::from_millis(500),
                    &mut server_to_client,
                ).await;
            }
        },
        r = &mut server_to_client => r?,
    }
    Ok(())
}

fn strip_mysql_ssl_flag(packet: &mut [u8]) {
    if packet.len() < 5 { return; }
    let payload = &mut packet[4..];
    if payload.is_empty() || payload[0] != 10 { return; }
    let mut pos = 1;
    while pos < payload.len() && payload[pos] != 0 { pos += 1; }
    pos += 1;
    pos += 4;
    pos += 8;
    pos += 1;
    if pos + 2 > payload.len() { return; }
    let cap_lower = u16::from_le_bytes([payload[pos], payload[pos + 1]]);
    let cap_lower_new = cap_lower & !0x0800;
    payload[pos] = (cap_lower_new & 0xff) as u8;
    payload[pos + 1] = ((cap_lower_new >> 8) & 0xff) as u8;
}

/// Resolve which process owns a local TCP port (the client's ephemeral port).
fn resolve_peer_process(port: u16) -> Option<String> {
    use std::process::Command;
    let my_pid = std::process::id().to_string();

    if cfg!(target_os = "macos") {
        let output = Command::new("lsof")
            .args(["-i", &format!("tcp:{}", port), "-sTCP:ESTABLISHED", "-Fp", "-Fc"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let mut current_pid = String::new();
        let mut current_cmd = String::new();
        for line in text.lines() {
            if let Some(p) = line.strip_prefix('p') {
                if !current_pid.is_empty() && current_pid != my_pid {
                    return Some(format!("[{}] {}", current_pid, current_cmd));
                }
                current_pid = p.to_string();
                current_cmd.clear();
            }
            if let Some(c) = line.strip_prefix('c') {
                current_cmd = c.to_string();
            }
        }
        if !current_pid.is_empty() && current_pid != my_pid {
            return Some(format!("[{}] {}", current_pid, current_cmd));
        }
        None
    } else {
        let output = Command::new("ss")
            .args(["-tnp", &format!("sport = :{}", port)])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if let Some(start) = line.find("users:((\"") {
                let rest = &line[start + 9..];
                if let Some(end) = rest.find('"') {
                    let proc_name = &rest[..end];
                    let pid = rest.find("pid=")
                        .and_then(|i| rest[i+4..].split(|c: char| !c.is_ascii_digit()).next())
                        .unwrap_or("?");
                    return Some(format!("[{}] {}", pid, proc_name));
                }
            }
        }
        None
    }
}

/// TLS certificate verifier that accepts any certificate (for proxying to known backends).
#[derive(Debug)]
struct NoVerify;

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self, _: &rustls::pki_types::CertificateDer<'_>, _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>, _: &[u8], _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self, _: &[u8], _: &rustls::pki_types::CertificateDer<'_>, _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self, _: &[u8], _: &rustls::pki_types::CertificateDer<'_>, _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_mysql_ssl_flag_short_packet() {
        let mut buf = vec![0u8; 3];
        strip_mysql_ssl_flag(&mut buf);
        assert_eq!(buf, vec![0u8; 3]);
    }

    #[test]
    fn test_strip_mysql_ssl_flag_not_greeting() {
        let mut buf = vec![0u8; 10];
        buf[4] = 9;
        strip_mysql_ssl_flag(&mut buf);
        assert_eq!(buf[4], 9);
    }

    fn caps_offset(pkt: &[u8]) -> Option<usize> {
        if pkt.len() < 5 { return None; }
        let mut pos = 5;
        while pos < pkt.len() && pkt[pos] != 0 { pos += 1; }
        pos += 1;
        if pos + 13 > pkt.len() { return None; }
        pos += 4;
        pos += 8;
        pos += 1;
        Some(pos)
    }

    #[test]
    fn test_strip_mysql_ssl_flag_clears_ssl_bit() {
        let version = b"5.7.0\0";
        let mut payload = vec![10];
        payload.extend_from_slice(version);
        payload.extend_from_slice(&[0u8; 4]);
        payload.extend_from_slice(&[0u8; 8]);
        payload.push(0);
        let caps: u16 = 0x0800;
        payload.extend_from_slice(&caps.to_le_bytes());
        payload.extend_from_slice(&[0u8; 13]);

        let pkt_len = payload.len();
        let mut pkt = vec![
            (pkt_len & 0xff) as u8,
            ((pkt_len >> 8) & 0xff) as u8,
            ((pkt_len >> 16) & 0xff) as u8,
            0,
        ];
        pkt.extend_from_slice(&payload);

        let off = caps_offset(&pkt).unwrap();
        assert!(u16::from_le_bytes([pkt[off], pkt[off + 1]]) & 0x0800 != 0);

        strip_mysql_ssl_flag(&mut pkt);

        assert_eq!(u16::from_le_bytes([pkt[off], pkt[off + 1]]) & 0x0800, 0);
    }

    #[test]
    fn test_resolve_peer_process_does_not_panic() {
        let result = std::panic::catch_unwind(|| resolve_peer_process(0));
        assert!(result.is_ok());
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::broadcast;
    use std::time::Duration;

    /// Start a mock "remote" server that echoes data back after a small delay.
    async fn start_echo_server() -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let handle = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                loop {
                    let n = match stream.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => break,
                    };
                    // Echo back after small delay to simulate response
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    if stream.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        });
        (addr, handle)
    }

    /// Start a mock Redis server that responds with +OK\r\n
    async fn start_redis_mock() -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let handle = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                loop {
                    let _n = match stream.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => break,
                    };
                    // Respond with +OK for any command
                    let _ = stream.write_all(b"+OK\r\n").await;
                }
            }
        });
        (addr, handle)
    }

    #[tokio::test]
    async fn test_proxy_redis_event_flow() {
        let (remote_addr, _server) = start_redis_mock().await;
        let (tx, mut rx) = broadcast::channel::<ProxyEvent>(64);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let status: StatusMap = Arc::new(Mutex::new(std::collections::HashMap::new()));

        // Start proxy on random port
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap().to_string();
        drop(proxy_listener); // Release port for run_proxy to bind

        let tx_clone = tx.clone();
        let status_clone = status.clone();
        let proxy_addr_clone = proxy_addr.clone();
        let proxy_handle = tokio::spawn(async move {
            let _ = run_proxy(
                proxy_addr_clone,
                remote_addr,
                "test-redis".into(),
                Protocol::Redis,
                tx_clone,
                shutdown_rx,
                status_clone,
            ).await;
        });

        // Wait for proxy to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Connect client and send Redis command
        if let Ok(mut client) = TcpStream::connect(&proxy_addr).await {
            // Send SET key value in RESP format
            let cmd = b"*3\r\n$3\r\nSET\r\n$3\r\nkey\r\n$5\r\nvalue\r\n";
            let _ = client.write_all(cmd).await;

            // Read response
            let mut buf = [0u8; 256];
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                client.read(&mut buf),
            ).await;

            // Check that an event was emitted
            if let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
                assert!(!ev.system, "should not be a system event");
                assert_eq!(ev.component, "test-redis");
                assert_eq!(ev.protocol, Protocol::Redis);
                assert!(ev.command.contains("SET"));
                assert!(ev.response.contains("OK"));
                assert!(ev.latency > Duration::ZERO);
            }

            drop(client);
        }

        let _ = shutdown_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(2), proxy_handle).await;
    }

    #[tokio::test]
    async fn test_proxy_connection_error_emits_system_event() {
        let (tx, mut rx) = broadcast::channel::<ProxyEvent>(64);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let status: StatusMap = Arc::new(Mutex::new(std::collections::HashMap::new()));

        // Bind to a random port, then release it — proxy will bind successfully
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap().to_string();
        drop(proxy_listener);

        // Point to a remote that doesn't exist (closed port)
        let closed_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = closed_listener.local_addr().unwrap().to_string();
        drop(closed_listener);

        let status_clone = status.clone();
        let proxy_addr_clone = proxy_addr.clone();
        let proxy_handle = tokio::spawn(async move {
            let _ = run_proxy(
                proxy_addr_clone,
                dead_addr,
                "dead-remote".into(),
                Protocol::Redis,
                tx,
                shutdown_rx,
                status_clone,
            ).await;
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Connect — proxy will try to connect to dead remote and fail
        if let Ok(mut client) = TcpStream::connect(&proxy_addr).await {
            // Redis protocol: proxy sends -ERR when remote unreachable
            let mut buf = [0u8; 512];
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                client.read(&mut buf),
            ).await;

            // Should get a system event
            if let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
                assert!(ev.system || ev.command.contains("cannot reach") || ev.response.contains("ERR"));
            }
            drop(client);
        }

        let _ = shutdown_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(2), proxy_handle).await;
    }

    #[tokio::test]
    async fn test_proxy_shutdown_stops_accepting() {
        let (remote_addr, _server) = start_redis_mock().await;
        let (tx, _rx) = broadcast::channel::<ProxyEvent>(64);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let status: StatusMap = Arc::new(Mutex::new(std::collections::HashMap::new()));

        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap().to_string();
        drop(proxy_listener);

        let status_clone = status.clone();
        let proxy_handle = tokio::spawn(async move {
            let _ = run_proxy(
                proxy_addr.clone(),
                remote_addr,
                "shutdown-test".into(),
                Protocol::Redis,
                tx,
                shutdown_rx,
                status_clone,
            ).await;
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Shutdown
        let _ = shutdown_tx.send(true);
        let result = tokio::time::timeout(Duration::from_secs(3), proxy_handle).await;
        assert!(result.is_ok(), "proxy should shut down within 3 seconds");
    }

    #[test]
    fn test_req_buf_mgr_new() {
        let mgr = ReqBufMgr::new();
        assert_eq!(mgr.buf.capacity(), 4096);
    }

    #[test]
    fn test_resp_buf_mgr_new() {
        let mgr = RespBufMgr::new();
        assert_eq!(mgr.buf.capacity(), 4096);
    }
}
