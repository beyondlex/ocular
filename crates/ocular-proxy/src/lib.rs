use anyhow::Result;
use ocular_protocol::{Direction, Protocol, parse_request, parse_response};
use std::sync::Arc;
use std::time::{Instant, SystemTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex};
use tracing::{info, warn, error, debug};

pub use ocular_protocol::ProxyEvent;

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
            error!(
                component = %name,
                remote = %remote_addr,
                error = %e,
                "failed to connect to remote — is the service running?"
            );
            if protocol == Protocol::Redis {
                let err_msg = format!("-ERR ocular proxy: cannot reach {} ({})\r\n", remote_addr, e);
                let _ = client.write_all(err_msg.as_bytes()).await;
            }
            return Err(e.into());
        }
    };

    // For MySQL: intercept the server greeting and strip SSL capability
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

    let last_req_time: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));

    let name_req = name.to_string();
    let name_resp = name.to_string();
    let tx_req = tx.clone();
    let tx_resp = tx.clone();
    let req_time_w = last_req_time.clone();
    let req_time_r = last_req_time;

    let client_to_server = async move {
        let mut buf = [0u8; 65536];
        loop {
            let n = cr.read(&mut buf).await?;
            if n == 0 { break; }
            let data = &buf[..n];
            if let Some(summary) = parse_request(protocol, data) {
                let now = Instant::now();
                *req_time_w.lock().await = Some(now);
                debug!(component = %name_req, direction = "request", %summary);
                let _ = tx_req.send(ProxyEvent {
                    timestamp: SystemTime::now(),
                    component: name_req.clone(),
                    protocol,
                    direction: Direction::Request,
                    summary,
                    raw: data.to_vec(),
                    latency: None,
                });
            }
            sw.write_all(data).await?;
        }
        Ok::<_, anyhow::Error>(())
    };

    let server_to_client = async move {
        let mut buf = [0u8; 65536];
        loop {
            let n = sr.read(&mut buf).await?;
            if n == 0 { break; }
            let data = &buf[..n];
            if let Some(summary) = parse_response(protocol, data) {
                let latency = req_time_r.lock().await.take().map(|t| t.elapsed());
                // For MySQL, only emit response if there was a matching request
                if protocol == Protocol::Mysql && latency.is_none() {
                    cw.write_all(data).await?;
                    continue;
                }
                debug!(component = %name_resp, direction = "response", %summary, ?latency);
                let _ = tx_resp.send(ProxyEvent {
                    timestamp: SystemTime::now(),
                    component: name_resp.clone(),
                    protocol,
                    direction: Direction::Response,
                    summary,
                    raw: data.to_vec(),
                    latency,
                });
            }
            cw.write_all(data).await?;
        }
        Ok::<_, anyhow::Error>(())
    };

    tokio::select! {
        r = client_to_server => r?,
        r = server_to_client => r?,
    }
    Ok(())
}

/// Strip the CLIENT_SSL (0x0800) capability flag from a MySQL server greeting packet.
/// This forces the client to use plaintext, allowing the proxy to parse traffic.
///
/// MySQL greeting format (Protocol::HandshakeV10):
///   [4-byte header][protocol_version(1)][server_version(null-terminated)]
///   [thread_id(4)][auth_plugin_data_part1(8)][filler(1)]
///   [capability_flags_lower(2)] <-- we clear bit 11 (0x0800) here
///   ...
fn strip_mysql_ssl_flag(packet: &mut Vec<u8>) {
    if packet.len() < 5 {
        return;
    }
    // Skip 4-byte header
    let payload = &mut packet[4..];
    if payload.is_empty() || payload[0] != 10 {
        // Not a HandshakeV10 packet
        return;
    }
    // Skip protocol version (1 byte)
    let mut pos = 1;
    // Skip server version (null-terminated string)
    while pos < payload.len() && payload[pos] != 0 {
        pos += 1;
    }
    pos += 1; // skip null terminator
    // Skip thread_id (4 bytes)
    pos += 4;
    // Skip auth_plugin_data_part_1 (8 bytes)
    pos += 8;
    // Skip filler (1 byte)
    pos += 1;
    // Now at capability_flags_lower (2 bytes, little-endian)
    if pos + 2 > payload.len() {
        return;
    }
    let cap_lower = u16::from_le_bytes([payload[pos], payload[pos + 1]]);
    // Clear CLIENT_SSL flag (bit 11 = 0x0800)
    let cap_lower_new = cap_lower & !0x0800;
    payload[pos] = (cap_lower_new & 0xff) as u8;
    payload[pos + 1] = ((cap_lower_new >> 8) & 0xff) as u8;
    debug!(original = format!("0x{:04x}", cap_lower), modified = format!("0x{:04x}", cap_lower_new), "stripped SSL from MySQL greeting");
}
