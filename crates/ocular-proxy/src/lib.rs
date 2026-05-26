use anyhow::Result;
use ocular_protocol::{Protocol, mysql::mysql_response_complete, postgres::postgres_response_complete, parse_request, parse_response, extract_full_command, format_response_detail, parse_amqp_frame, parse_amqp_request_full, is_async_method, amqp_frame_len};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt, AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tracing::{info, warn, error, debug};

pub use ocular_protocol::ProxyEvent;

/// Connection state for a proxy component, shared between proxy and TUI
#[derive(Clone, Default)]
pub struct ConnectionState {
    pub active_connections: usize,
    pub has_connector: bool,
    pub last_error: Option<String>,
    pub last_active_at: Option<SystemTime>,
}

/// Shared map from component name to connection state
pub type StatusMap = Arc<Mutex<std::collections::HashMap<String, ConnectionState>>>;

/// Pending request info
struct PendingRequest {
    timestamp: SystemTime,
    instant: Instant,
    command: String,
    full_command: String,
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
    // This lets the server know the connection won't be encrypted (may affect auth requirements).
    if protocol == Protocol::Postgres {
        let mut buf = [0u8; 256];
        let n = client.read(&mut buf).await?;
        if n == 0 { return Ok(()); }
        let data = &buf[..n];
        let neg_code = if n >= 8 {
            u32::from_be_bytes([data[4], data[5], data[6], data[7]])
        } else { 0 };
        if neg_code == 80877103 || neg_code == 80877104 {
            // Forward negotiation to server so it knows the connection state
            sw.write_all(data).await?;
            // Read server's response (single byte: N or S)
            let mut resp = [0u8; 1];
            let rn = sr.read(&mut resp).await?;
            if rn == 0 { return Ok(()); }
            // Always tell client: no SSL/GSS (force plaintext for proxy to parse)
            client.write_all(&[b'N']).await?;
        } else {
            // Not a negotiation request, forward as Startup
            sw.write_all(data).await?;
        }
    }

    let (mut cr, mut cw) = client.split();

    let pending: Arc<Mutex<Option<PendingRequest>>> = Arc::new(Mutex::new(None));

    let name_req = name.to_string();
    let name_resp = name.to_string();
    let tx_req = tx.clone();
    let tx_resp = tx.clone();
    let pending_w = pending.clone();
    let pending_final = pending.clone();
    let pending_r = pending;
    let process_info = process;

    let process_req = process_info.clone();
    let src_req = src.clone();
    let dest_req = dest.clone();
    let src_resp = src.clone();
    let dest_resp = dest;
    let client_to_server = async move {
        let mut buf = [0u8; 65536];
        let mut http_req_buf: Vec<u8> = Vec::with_capacity(4096);
        let mut memcached_req_buf: Vec<u8> = Vec::with_capacity(4096);
        let mut kafka_req_buf: Vec<u8> = Vec::with_capacity(4096);
        loop {
            let n = cr.read(&mut buf).await?;
            if n == 0 { break; }
            let data = &buf[..n];

            if protocol == Protocol::Amqp {
                // AMQP: loop through all frames in this read
                let mut pos = 0;
                while pos < data.len() {
                    let frame_data = &data[pos..];
                    let Some(flen) = amqp_frame_len(frame_data) else { break };
                    if let Some(frame) = parse_amqp_frame(frame_data) {
                        // Skip heartbeat — not a real request
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
                                    latency: std::time::Duration::ZERO,
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
                                });
                            }
                        }
                    }
                    pos += flen;
                }
            } else if protocol == Protocol::Http {
                http_req_buf.extend_from_slice(data);
                if ocular_protocol::http::http_request_complete(&http_req_buf) {
                    if let Some(command) = parse_request(protocol, &http_req_buf) {
                        let full_command = extract_full_command(protocol, &http_req_buf).unwrap_or_else(|| command.clone());
                        *pending_w.lock().unwrap() = Some(PendingRequest {
                            timestamp: SystemTime::now(),
                            instant: Instant::now(),
                            command,
                            full_command,
                        });
                    }
                    http_req_buf.clear();
                }
            } else if protocol == Protocol::Memcached {
                memcached_req_buf.extend_from_slice(data);
                while ocular_protocol::memcached::memcached_request_complete(&memcached_req_buf) {
                    // If there's already a pending request that won't get a response pairing,
                    // emit it as a standalone event
                    if let Some(prev) = pending_w.lock().unwrap().take() {
                        let _ = tx_req.send(ProxyEvent {
                            timestamp: prev.timestamp,
                            component: name_req.clone(),
                            protocol,
                            command: prev.command,
                            full_command: prev.full_command,
                            response: String::new(),
                            response_detail: String::new(),
                            latency: Duration::ZERO,
                            process: process_req.clone(),
                            src: Some(src_req.clone()),
                            dest: Some(dest_req.clone()),
                            system: false,
                        });
                    }
                    if let Some(command) = parse_request(protocol, &memcached_req_buf) {
                        let full_command = extract_full_command(protocol, &memcached_req_buf).unwrap_or_else(|| command.clone());
                        *pending_w.lock().unwrap() = Some(PendingRequest {
                            timestamp: SystemTime::now(),
                            instant: Instant::now(),
                            command,
                            full_command,
                        });
                    }
                    // Advance past this request
                    let s = std::str::from_utf8(&memcached_req_buf).unwrap_or("");
                    let first_crlf = s.find("\r\n").unwrap_or(0);
                    let line = &s[..first_crlf];
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    let cmd = parts.first().map(|c| c.to_uppercase()).unwrap_or_default();
                    let consumed = match cmd.as_str() {
                        "SET" | "ADD" | "REPLACE" | "APPEND" | "PREPEND" | "CAS" => {
                            let bytes: usize = parts.get(4).and_then(|b| b.parse().ok()).unwrap_or(0);
                            first_crlf + 2 + bytes + 2
                        }
                        _ => first_crlf + 2,
                    };
                    memcached_req_buf = memcached_req_buf[consumed..].to_vec();
                }
            } else if protocol == Protocol::Kafka {
                kafka_req_buf.extend_from_slice(data);
                while ocular_protocol::kafka::kafka_frame_complete(&kafka_req_buf) {
                    let frame_len = i32::from_be_bytes([kafka_req_buf[0], kafka_req_buf[1], kafka_req_buf[2], kafka_req_buf[3]]) as usize + 4;
                    let frame = &kafka_req_buf[..frame_len];
                    if let Some(command) = parse_request(protocol, frame) {
                        let full_command = extract_full_command(protocol, frame).unwrap_or_else(|| command.clone());
                        *pending_w.lock().unwrap() = Some(PendingRequest {
                            timestamp: SystemTime::now(),
                            instant: Instant::now(),
                            command,
                            full_command,
                        });
                    }
                    kafka_req_buf = kafka_req_buf[frame_len..].to_vec();
                }
            } else if protocol == Protocol::Postgres {
                // Postgres: scan all messages in this read, keep SQL from Q/P only
                let mut pos = 0;
                while pos < data.len() {
                    let first = data[pos];
                    let is_typed = matches!(first, b'Q' | b'P' | b'B' | b'E' | b'D' | b'S' | b'X' | b'C' | b'p' | b'H' | b'F' | b'd' | b'c' | b'f');
                    if !is_typed { break; }
                    if pos + 5 > data.len() { break; }
                    let len = u32::from_be_bytes([data[pos+1], data[pos+2], data[pos+3], data[pos+4]]) as usize;
                    let end = pos + 1 + len;
                    if end > data.len() { break; }
                    // Only set pending for Q (simple query) or P (Parse with SQL)
                    if first == b'Q' || first == b'P' {
                        let msg = &data[pos..end];
                        if let Some(command) = parse_request(protocol, msg) {
                            let full_command = extract_full_command(protocol, msg).unwrap_or_else(|| command.clone());
                            *pending_w.lock().unwrap() = Some(PendingRequest {
                                timestamp: SystemTime::now(),
                                instant: Instant::now(),
                                command,
                                full_command,
                            });
                        }
                    }
                    pos = end;
                }
            } else if let Some(command) = parse_request(protocol, data) {
                let full_command = extract_full_command(protocol, data).unwrap_or_else(|| command.clone());
                debug!(component = %name_req, %command);
                *pending_w.lock().unwrap() = Some(PendingRequest {
                    timestamp: SystemTime::now(),
                    instant: Instant::now(),
                    command,
                    full_command,
                });
            }

            sw.write_all(data).await?;
        }
        Ok::<_, anyhow::Error>(())
    };

    let process_mysql = process_info.clone();
    let server_to_client = async move {
        let mut buf = [0u8; 65536];
        let mut mysql_buf: Vec<u8> = Vec::with_capacity(4096);
        let mut http_resp_buf: Vec<u8> = Vec::with_capacity(4096);
        let mut memcached_resp_buf: Vec<u8> = Vec::with_capacity(4096);
        let mut kafka_resp_buf: Vec<u8> = Vec::with_capacity(4096);
        let mut pg_resp_buf: Vec<u8> = Vec::with_capacity(4096);
        let mut awaiting_response = false;
        let mut memcached_awaiting = false;
        loop {
            let n = sr.read(&mut buf).await?;
            if n == 0 { break; }
            let data = &buf[..n];
            cw.write_all(data).await?;

            if protocol == Protocol::Mysql {
                let has_pending = pending_r.lock().unwrap().is_some();
                if has_pending || awaiting_response {
                    awaiting_response = true;
                    mysql_buf.extend_from_slice(data);
                    if mysql_response_complete(&mysql_buf) {
                        if let Some(req) = pending_r.lock().unwrap().take() {
                            let latency = req.instant.elapsed();
                            let response = parse_response(protocol, &mysql_buf).unwrap_or_default();
                            let response_detail = format_response_detail(protocol, &mysql_buf).unwrap_or_default();
                            let _ = tx_resp.send(ProxyEvent {
                                timestamp: req.timestamp,
                                component: name_resp.clone(),
                                protocol,
                                command: req.command,
                                full_command: req.full_command,
                                response,
                                response_detail,
                                latency,
                                process: process_mysql.clone(),
                                src: Some(src_resp.clone()),
                                dest: Some(dest_resp.clone()),
                    system: false,
                            });
                        }
                        mysql_buf.clear();
                        awaiting_response = false;
                    }
                }
            } else if protocol == Protocol::Http {
                http_resp_buf.extend_from_slice(data);
                if ocular_protocol::http::http_response_complete(&http_resp_buf) {
                    if let Some(req) = pending_r.lock().unwrap().take() {
                        let latency = req.instant.elapsed();
                        let response = parse_response(protocol, &http_resp_buf).unwrap_or_default();
                        let response_detail = format_response_detail(protocol, &http_resp_buf).unwrap_or_else(|| response.clone());
                        let _ = tx_resp.send(ProxyEvent {
                            timestamp: req.timestamp,
                            component: name_resp.clone(),
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
                    http_resp_buf.clear();
                }
            } else if protocol == Protocol::Amqp {
                // AMQP: loop through all server frames
                let mut pos = 0;
                while pos < data.len() {
                    let frame_data = &data[pos..];
                    let Some(flen) = amqp_frame_len(frame_data) else { break };
                    if let Some(frame) = parse_amqp_frame(frame_data) {
                        // Skip content header and body frames — handled below with method
                        if frame.frame_type == 2 || frame.frame_type == 3 {
                            pos += flen;
                            continue;
                        }
                        // Heartbeat: skip
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
                                // Header frame, skip
                            } else if pf.frame_type == 3 {
                                // Body frame
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
                        let mut response_detail = format_response_detail(protocol, frame_data).unwrap_or_else(|| response.clone());
                        if !body_text.is_empty() {
                            response = format!("{} | {}", response, body_text);
                            response_detail = format!("{}\nBody: {}", response_detail, body_text);
                        }
                        let _ = tx_resp.send(ProxyEvent {
                            timestamp: req.timestamp,
                            component: name_resp.clone(),
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
                        // Server-initiated method (e.g. Basic.Deliver) — emit as standalone
                        if let Some(ref method) = frame.method {
                            let response = if body_text.is_empty() { String::new() } else { body_text.clone() };
                            let response_detail = if body_text.is_empty() { String::new() } else { body_text.clone() };
                            let command = method.summary.clone();
                            let _ = tx_resp.send(ProxyEvent {
                                timestamp: SystemTime::now(),
                                component: name_resp.clone(),
                                protocol,
                                command,
                                full_command: method.detail.clone(),
                                response,
                                response_detail,
                                latency: std::time::Duration::ZERO,
                                process: process_info.clone(),
                                    src: Some(dest_resp.clone()),
                                    dest: Some(src_resp.clone()),
                    system: false,
                            });
                        }
                    }
                    // Advance past the method frame + any header/body frames we consumed
                    pos = peek;
                }
            } else if protocol == Protocol::Postgres {
                // Buffer until ReadyForQuery, then emit single event
                pg_resp_buf.extend_from_slice(data);
                if postgres_response_complete(&pg_resp_buf) {
                    if let Some(req) = pending_r.lock().unwrap().take() {
                        let latency = req.instant.elapsed();
                        let response = parse_response(protocol, &pg_resp_buf).unwrap_or_default();
                        let response_detail = format_response_detail(protocol, &pg_resp_buf).unwrap_or_else(|| response.clone());
                        let _ = tx_resp.send(ProxyEvent {
                            timestamp: req.timestamp,
                            component: name_resp.clone(),
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
                    pg_resp_buf.clear();
                }
            } else if protocol == Protocol::Memcached {
                let has_pending = pending_r.lock().unwrap().is_some();
                if has_pending || memcached_awaiting {
                    memcached_awaiting = true;
                    memcached_resp_buf.extend_from_slice(data);
                    if ocular_protocol::memcached::memcached_response_complete(&memcached_resp_buf) {
                        if let Some(req) = pending_r.lock().unwrap().take() {
                            let latency = req.instant.elapsed();
                            let response = parse_response(protocol, &memcached_resp_buf).unwrap_or_default();
                            let response_detail = format_response_detail(protocol, &memcached_resp_buf).unwrap_or_else(|| response.clone());
                            let _ = tx_resp.send(ProxyEvent {
                                timestamp: req.timestamp,
                                component: name_resp.clone(),
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
                        memcached_resp_buf.clear();
                        memcached_awaiting = false;
                    }
                }
            } else if protocol == Protocol::Kafka {
                kafka_resp_buf.extend_from_slice(data);
                while ocular_protocol::kafka::kafka_frame_complete(&kafka_resp_buf) {
                    let frame_len = i32::from_be_bytes([kafka_resp_buf[0], kafka_resp_buf[1], kafka_resp_buf[2], kafka_resp_buf[3]]) as usize + 4;
                    if let Some(req) = pending_r.lock().unwrap().take() {
                        let latency = req.instant.elapsed();
                        let response = parse_response(protocol, &kafka_resp_buf[..frame_len]).unwrap_or_default();
                        let response_detail = format_response_detail(protocol, &kafka_resp_buf[..frame_len]).unwrap_or_else(|| response.clone());
                        let _ = tx_resp.send(ProxyEvent {
                            timestamp: req.timestamp,
                            component: name_resp.clone(),
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
                    kafka_resp_buf = kafka_resp_buf[frame_len..].to_vec();
                }
            } else {
                // Redis/MongoDB: single request/response per read
                if let Some(req) = pending_r.lock().unwrap().take() {
                    let latency = req.instant.elapsed();
                    let response = parse_response(protocol, data).unwrap_or_default();
                    let response_detail = format_response_detail(protocol, data).unwrap_or_else(|| response.clone());
                    let _ = tx_resp.send(ProxyEvent {
                        timestamp: req.timestamp,
                        component: name_resp.clone(),
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
        // lsof -i tcp:PORT -sTCP:ESTABLISHED -Fp -Fc
        // Returns multiple process entries; skip our own PID
        let output = Command::new("lsof")
            .args(["-i", &format!("tcp:{}", port), "-sTCP:ESTABLISHED", "-Fp", "-Fc"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let mut current_pid = String::new();
        let mut current_cmd = String::new();
        for line in text.lines() {
            if let Some(p) = line.strip_prefix('p') {
                // Save previous entry if it wasn't us
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
        // Check last entry
        if !current_pid.is_empty() && current_pid != my_pid {
            return Some(format!("[{}] {}", current_pid, current_cmd));
        }
        None
    } else {
        // Linux: ss -tnp sport = :PORT
        let output = Command::new("ss")
            .args(["-tnp", &format!("sport = :{}", port)])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        // Parse: users:(("process_name",pid=1234,fd=5))
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

    /// Find the capability flags offset in a MySQL greeting packet
    fn caps_offset(pkt: &[u8]) -> Option<usize> {
        if pkt.len() < 5 { return None; }
        let mut pos = 5;
        // Skip null-terminated server version
        while pos < pkt.len() && pkt[pos] != 0 { pos += 1; }
        pos += 1; // null
        if pos + 13 > pkt.len() { return None; }
        pos += 4; // thread id
        pos += 8; // salt part 1
        pos += 1; // filler
        Some(pos)
    }

    #[test]
    fn test_strip_mysql_ssl_flag_clears_ssl_bit() {
        let version = b"5.7.0\0";
        let mut payload = vec![10]; // protocol version
        payload.extend_from_slice(version);
        payload.extend_from_slice(&[0u8; 4]); // thread id
        payload.extend_from_slice(&[0u8; 8]); // salt part 1
        payload.push(0); // filler
        let caps: u16 = 0x0800; // SSL flag set
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
