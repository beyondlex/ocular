use anyhow::Result;
use ocular_protocol::{Protocol, parse_request, parse_response, extract_full_command, format_response_detail, parse_amqp_frame, parse_amqp_request_full, is_async_method, amqp_frame_len};
use std::sync::Arc;
use std::time::{Instant, SystemTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt, AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex};
use tracing::{info, warn, error, debug};

pub use ocular_protocol::ProxyEvent;

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
) -> Result<()> {
    let listener = TcpListener::bind(&listen_addr).await?;
    info!(component = %name, listen = %listen_addr, remote = %remote_addr, ?protocol, "proxy listening");

    loop {
        let (client, peer) = listener.accept().await?;
        debug!(component = %name, peer = %peer, "new client connection");
        let remote = remote_addr.clone();
        let name = name.clone();
        let tx = tx.clone();
        let process = resolve_peer_process(peer.port());
        let peer_addr = peer.to_string();
        let remote_for_conn = remote.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(client, &remote, &name, protocol, &tx, process, peer_addr, remote_for_conn).await {
                warn!(component = %name, remote = %remote, error = %e, "connection ended with error");
            }
        });
    }
}

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

    // For PostgreSQL: handle SSL negotiation before normal flow
    if protocol == Protocol::Postgres {
        let mut buf = [0u8; 256];
        let n = client.read(&mut buf).await?;
        if n == 0 { return Ok(()); }
        let data = &buf[..n];
        // Forward SSLRequest to server
        sw.write_all(data).await?;
        // Read server's SSL response (single byte N or S)
        let mut resp = [0u8; 1];
        let rn = sr.read(&mut resp).await?;
        if rn == 0 { return Ok(()); }
        // Forward to client
        client.write_all(&resp[..rn]).await?;
        // Emit SSLRequest event
        let command = parse_request(protocol, data).unwrap_or_else(|| "SSLRequest".into());
        let response = if resp[0] == b'N' { "SSLResponse: No" } else { "SSLResponse: Yes" };
        let _ = tx.send(ProxyEvent {
            timestamp: SystemTime::now(),
            component: name.to_string(),
            protocol,
            command: command.clone(),
            full_command: command,
            response: response.into(),
            response_detail: response.into(),
            latency: std::time::Duration::ZERO,
            process: process.clone(),
            src: Some(src.clone()),
            dest: Some(dest.clone()),
        });
        // If server said 'S' (SSL), we'd need to upgrade — but we don't support that
        // Most local setups respond 'N'
    }

    let (mut cr, mut cw) = client.split();

    let pending: Arc<Mutex<Option<PendingRequest>>> = Arc::new(Mutex::new(None));

    let name_req = name.to_string();
    let name_resp = name.to_string();
    let tx_req = tx.clone();
    let tx_resp = tx.clone();
    let pending_w = pending.clone();
    let pending_r = pending;
    let process_info = process;

    let process_req = process_info.clone();
    let src_req = src.clone();
    let dest_req = dest.clone();
    let src_resp = src.clone();
    let dest_resp = dest;
    let client_to_server = async move {
        let mut buf = [0u8; 65536];
        let mut http_req_buf: Vec<u8> = Vec::new();
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
                                });
                            } else {
                                debug!(component = %name_req, command = %method.summary);
                                *pending_w.lock().await = Some(PendingRequest {
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
                        *pending_w.lock().await = Some(PendingRequest {
                            timestamp: SystemTime::now(),
                            instant: Instant::now(),
                            command,
                            full_command,
                        });
                    }
                    http_req_buf.clear();
                }
            } else if let Some(command) = parse_request(protocol, data) {
                let full_command = extract_full_command(protocol, data).unwrap_or_else(|| command.clone());
                debug!(component = %name_req, %command);
                *pending_w.lock().await = Some(PendingRequest {
                    timestamp: SystemTime::now(),
                    instant: Instant::now(),
                    command,
                    full_command,
                });
            } else if protocol == Protocol::Postgres && n > 0 {
                info!(component = %name_req, bytes = n, first_byte = format!("0x{:02x}", data[0]), "pg client→server UNPARSED");
            }

            sw.write_all(data).await?;
        }
        Ok::<_, anyhow::Error>(())
    };

    let process_mysql = process_info.clone();
    let server_to_client = async move {
        let mut buf = [0u8; 65536];
        let mut mysql_buf: Vec<u8> = Vec::new();
        let mut http_resp_buf: Vec<u8> = Vec::new();
        let mut awaiting_response = false;
        loop {
            let n = sr.read(&mut buf).await?;
            if n == 0 { break; }
            let data = &buf[..n];
            cw.write_all(data).await?;

            if protocol == Protocol::Mysql {
                let has_pending = pending_r.lock().await.is_some();
                if has_pending || awaiting_response {
                    awaiting_response = true;
                    mysql_buf.extend_from_slice(data);
                    if mysql_response_complete(&mysql_buf) {
                        if let Some(req) = pending_r.lock().await.take() {
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
                            });
                        }
                        mysql_buf.clear();
                        awaiting_response = false;
                    }
                }
            } else if protocol == Protocol::Http {
                http_resp_buf.extend_from_slice(data);
                if ocular_protocol::http::http_response_complete(&http_resp_buf) {
                    if let Some(req) = pending_r.lock().await.take() {
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

                    if let Some(req) = pending_r.lock().await.take() {
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
                            });
                        }
                    }
                    // Advance past the method frame + any header/body frames we consumed
                    pos = peek;
                }
            } else if protocol == Protocol::Postgres {
                // Postgres: only pair with meaningful responses, skip setup noise
                let first = data[0];
                info!(component = %name_resp, bytes = n, first_byte = format!("0x{:02x}", first),
                    hex_head = format!("{:02x?}", &data[..n.min(20)]), "pg server→client");
                // Use parse_postgres_response which scans all messages and prioritizes errors
                let is_meaningful = matches!(first, b'C' | b'E' | b'T' | b'Z' | b'I' | b'D' | b'R');
                if is_meaningful {
                    if let Some(req) = pending_r.lock().await.take() {
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
                        });
                    }
                }
                // ParameterStatus (S), BackendKeyData (K), etc. are silently skipped
            } else {
                // Redis/MongoDB: single request/response per read
                if let Some(req) = pending_r.lock().await.take() {
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
                    });
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    };

    tokio::select! {
        r = client_to_server => r?,
        r = server_to_client => r?,
    }
    Ok(())
}

fn mysql_response_complete(buf: &[u8]) -> bool {
    if buf.len() < 5 { return false; }
    let first_marker = buf[4];
    match first_marker {
        0x00 | 0xff => return true,
        _ => {}
    }
    let mut pos = 0;
    let mut last_marker = 0u8;
    let mut last_pkt_len = 0usize;
    while pos + 4 <= buf.len() {
        let pkt_len = (buf[pos] as usize) | (buf[pos+1] as usize) << 8 | (buf[pos+2] as usize) << 16;
        let end = pos + 4 + pkt_len;
        if end > buf.len() { break; }
        if pkt_len > 0 {
            last_marker = buf[pos + 4];
            last_pkt_len = pkt_len;
        }
        pos = end;
    }
    (last_marker == 0xfe && last_pkt_len < 9) || (last_marker == 0x00 && last_pkt_len < 16 && pos == buf.len())
}

fn strip_mysql_ssl_flag(packet: &mut Vec<u8>) {
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
