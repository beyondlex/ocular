use anyhow::Result;
use ocular_protocol::{Protocol, parse_request, parse_response, extract_full_command, format_response_detail};
use std::sync::Arc;
use std::time::{Instant, SystemTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
        tokio::spawn(async move {
            if let Err(e) = handle_conn(client, &remote, &name, protocol, &tx).await {
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
) -> Result<()> {
    let mut server = match TcpStream::connect(remote_addr).await {
        Ok(s) => {
            debug!(component = %name, remote = %remote_addr, "connected to remote");
            s
        }
        Err(e) => {
            error!(component = %name, remote = %remote_addr, error = %e,
                "failed to connect to remote — is the service running?");
            if protocol == Protocol::Redis {
                let err_msg = format!("-ERR ocular proxy: cannot reach {} ({})\r\n", remote_addr, e);
                let _ = client.write_all(err_msg.as_bytes()).await;
            }
            return Err(e.into());
        }
    };

    // For MySQL: strip SSL from greeting
    if protocol == Protocol::Mysql {
        let mut greeting_buf = [0u8; 65536];
        let n = server.read(&mut greeting_buf).await?;
        if n == 0 { return Ok(()); }
        let mut greeting = greeting_buf[..n].to_vec();
        strip_mysql_ssl_flag(&mut greeting);
        client.write_all(&greeting).await?;
        debug!(component = %name, "forwarded MySQL greeting with SSL stripped");
    }

    let (mut cr, mut cw) = client.split();
    let (mut sr, mut sw) = server.split();

    let pending: Arc<Mutex<Option<PendingRequest>>> = Arc::new(Mutex::new(None));

    let name_req = name.to_string();
    let name_resp = name.to_string();
    let tx_resp = tx.clone();
    let pending_w = pending.clone();
    let pending_r = pending;

    let client_to_server = async move {
        let mut buf = [0u8; 65536];
        loop {
            let n = cr.read(&mut buf).await?;
            if n == 0 { break; }
            let data = &buf[..n];
            if let Some(command) = parse_request(protocol, data) {
                let full_command = extract_full_command(protocol, data).unwrap_or_else(|| command.clone());
                debug!(component = %name_req, %command);
                *pending_w.lock().await = Some(PendingRequest {
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

    let server_to_client = async move {
        let mut buf = [0u8; 65536];
        let mut mysql_buf: Vec<u8> = Vec::new();
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
                            });
                        }
                        mysql_buf.clear();
                        awaiting_response = false;
                    }
                }
            } else {
                // Redis: single request/response per read
                if let Some(req) = pending_r.lock().await.take() {
                    let latency = req.instant.elapsed();
                    let response = parse_response(protocol, data).unwrap_or_default();
                    let response_detail = response.clone();
                    let _ = tx_resp.send(ProxyEvent {
                        timestamp: req.timestamp,
                        component: name_resp.clone(),
                        protocol,
                        command: req.command,
                        full_command: req.full_command,
                        response,
                        response_detail,
                        latency,
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
