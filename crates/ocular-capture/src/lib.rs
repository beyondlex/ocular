mod stream;

use anyhow::{Context, Result};
use ocular_protocol::{
    extract_full_command, format_response_detail, get_handler, parse_request, parse_response,
    Protocol, ProxyEvent,
};
use std::collections::HashMap;
use std::time::{Instant, SystemTime};
use stream::{ConnKey, Direction, TcpStreamState};
use tokio::sync::broadcast;
use tracing::info;

/// Configuration for a single capture target.
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    pub name: String,
    pub protocol: Protocol,
    pub interface: String,
    pub remote: String, // host:port of the real service
}

/// Run the capture engine for a single target. Blocks until shutdown signal.
pub async fn run_capture(
    config: CaptureConfig,
    tx: broadcast::Sender<ProxyEvent>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    status: ocular_protocol::StatusMap,
) -> Result<()> {
    let remote_addr: std::net::SocketAddr = config
        .remote
        .parse()
        .with_context(|| format!("invalid remote address: {}", config.remote))?;
    let port = remote_addr.port();

    // Mark as active in status map
    {
        let mut map = status.lock().unwrap();
        let entry = map.entry(config.name.clone()).or_default();
        entry.has_connector = true;
    }

    // Open pcap on the specified interface
    let mut cap = pcap::Capture::from_device(config.interface.as_str())
        .with_context(|| format!("cannot open device {}", config.interface))?
        .promisc(true)
        .snaplen(65535)
        .timeout(100) // ms, allows periodic shutdown checks
        .open()
        .with_context(|| format!(
            "failed to activate capture on {} — permission denied. Run with sudo or: sudo chmod g+r /dev/bpf*",
            config.interface
        ))?;

    let filter = format!("tcp port {}", port);
    cap.filter(&filter, true)
        .with_context(|| format!("BPF filter failed: {}", filter))?;

    info!(
        component = %config.name,
        interface = %config.interface,
        filter = %filter,
        "capture started"
    );

    let datalink = cap.get_datalink();
    let handler = get_handler(config.protocol);
    let mut streams: HashMap<ConnKey, TcpStreamState> = HashMap::new();

    // Run blocking pcap loop in a dedicated thread
    let (pkt_tx, mut pkt_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4096);
    let _cap_thread = std::thread::spawn(move || {
        loop {
            match cap.next_packet() {
                Ok(packet) => {
                    if pkt_tx.blocking_send(packet.data.to_vec()).is_err() {
                        break; // receiver dropped
                    }
                }
                Err(pcap::Error::TimeoutExpired) => continue,
                Err(e) => {
                    tracing::error!(error = %e, "pcap read error");
                    break;
                }
            }
        }
    });

    loop {
        tokio::select! {
            Some(raw) = pkt_rx.recv() => {
                process_packet(
                    &raw,
                    remote_addr,
                    &config.name,
                    config.protocol,
                    handler,
                    &mut streams,
                    &tx,
                    datalink,
                    &status,
                );
            }
            _ = shutdown.changed() => {
                info!(component = %config.name, "capture shutting down");
                break;
            }
        }
    }

    drop(pkt_rx); // signal thread to stop
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_packet(
    raw: &[u8],
    remote_addr: std::net::SocketAddr,
    name: &str,
    protocol: Protocol,
    handler: &'static dyn ocular_protocol::ProtocolHandler,
    streams: &mut HashMap<ConnKey, TcpStreamState>,
    tx: &broadcast::Sender<ProxyEvent>,
    datalink: pcap::Linktype,
    status: &ocular_protocol::StatusMap,
) {
    use etherparse::SlicedPacket;

    // DLT_NULL (BSD loopback) = 0, DLT_EN10MB (Ethernet) = 1
    let packet = if datalink == pcap::Linktype(0) {
        match parse_loopback(raw) {
            Some(p) => p,
            None => {
                tracing::debug!(component = %name, "parse_loopback failed, raw_len={}", raw.len());
                return;
            }
        }
    } else {
        match SlicedPacket::from_ethernet(raw) {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!(component = %name, "from_ethernet failed: {}, raw_len={}", e, raw.len());
                return;
            }
        }
    };

    let (src_ip, dst_ip) = match packet.net {
        Some(etherparse::NetSlice::Ipv4(ref h)) => {
            (h.header().source_addr(), h.header().destination_addr())
        }
        _ => {
            tracing::debug!(component = %name, "no IPv4 header");
            return;
        }
    };

    let (src_port, dst_port, payload) = match packet.transport {
        Some(etherparse::TransportSlice::Tcp(ref tcp)) => {
            (tcp.source_port(), tcp.destination_port(), tcp.payload())
        }
        _ => {
            tracing::debug!(component = %name, "no TCP transport");
            return;
        }
    };

    if payload.is_empty() {
        // Check for FIN/RST to clean up streams
        if let Some(etherparse::TransportSlice::Tcp(ref tcp)) = packet.transport {
            let flags = tcp.slice();
            let fin = flags[13] & 0x01 != 0;
            let rst = flags[13] & 0x04 != 0;
            if fin || rst {
                let key = ConnKey::new(src_ip.into(), src_port, dst_ip.into(), dst_port);
                streams.remove(&key);
                streams.remove(&key.reverse());
            }
        }
        return;
    }

    let remote_ip: std::net::IpAddr = remote_addr.ip();
    let remote_port = remote_addr.port();

    tracing::debug!(
        component = %name,
        "pkt: {}:{} -> {}:{} payload={}B remote={}:{}",
        src_ip, src_port, dst_ip, dst_port, payload.len(), remote_ip, remote_port
    );

    // Determine direction: to remote = request, from remote = response
    let (direction, conn_key) = if std::net::IpAddr::from(dst_ip) == remote_ip && dst_port == remote_port {
        (Direction::Request, ConnKey::new(src_ip.into(), src_port, dst_ip.into(), dst_port))
    } else if std::net::IpAddr::from(src_ip) == remote_ip && src_port == remote_port {
        (Direction::Response, ConnKey::new(dst_ip.into(), dst_port, src_ip.into(), src_port))
    } else {
        tracing::debug!(component = %name, "direction mismatch — skipped");
        return; // not relevant
    };

    let stream = streams.entry(conn_key).or_insert_with(|| {
        let mut s = TcpStreamState::new();
        if protocol == Protocol::Mysql {
            s.handshake_done = false;
        }
        s
    });

    // MySQL: skip handshake phase packets.
    // Handshake may involve multiple rounds (e.g. caching_sha2_password):
    //   Server Greeting (seq=0, marker=0x0a) → Client Login (seq=1) →
    //   AuthSwitch (seq=2, 0xfe) → Client Auth (seq=3) → OK (seq=4, 0x00)
    // We detect handshake completion when server sends OK (0x00) with seq>0.
    // For mid-stream connections (missed handshake), a request with seq=0 is a real command.
    if protocol == Protocol::Mysql && !stream.handshake_done {
        if payload.len() >= 5 {
            let seq = payload[3];
            let marker = payload[4];
            tracing::debug!(
                component = %name,
                "mysql handshake: dir={:?} seq={} marker=0x{:02x} payload={}B",
                direction, seq, marker, payload.len()
            );
            match direction {
                Direction::Request if seq == 0 => {
                    // seq=0 request = real command (missed handshake)
                    stream.handshake_done = true;
                    tracing::debug!(component = %name, "mysql handshake skipped (mid-stream connection)");
                    // fall through to normal processing below
                }
                Direction::Response if seq == 0 && marker == 10 => {
                    // Server greeting
                    return;
                }
                Direction::Response if marker == 0x00 => {
                    // OK packet — auth success, handshake complete
                    stream.handshake_done = true;
                    tracing::debug!(component = %name, "mysql handshake done (OK at seq={})", seq);
                    return;
                }
                _ => {
                    // All other handshake packets: login, AuthSwitchRequest (0xfe),
                    // AuthMoreData (0x01), ERR (0xff), client auth responses
                    return;
                }
            }
        } else {
            tracing::debug!(component = %name, "mysql handshake: payload too short ({}B)", payload.len());
            return;
        }
    }

    match direction {
        Direction::Request => {
            stream.push_request(payload);
            // Try to parse complete request
            if handler.needs_request_buffering() && !handler.request_complete(&stream.request_buf) {
                return;
            }
            let buf = &stream.request_buf;
            if let Some(command) = parse_request(protocol, buf) {
                tracing::debug!(component = %name, "parsed request: {}", command);
                let full_command =
                    extract_full_command(protocol, buf).unwrap_or_else(|| command.clone());
                stream.pending_request = Some(PendingRequest {
                    timestamp: SystemTime::now(),
                    instant: Instant::now(),
                    command,
                    full_command,
                });
                stream.request_buf.clear();
            } else {
                tracing::debug!(
                    component = %name,
                    "parse_request returned None, buf={}B first_bytes={:02x?}",
                    stream.request_buf.len(),
                    &stream.request_buf[..stream.request_buf.len().min(16)]
                );
                // MySQL: if buffer contains a complete packet that we can't parse
                // (e.g. COM_FIELD_LIST), discard it to avoid polluting future commands.
                if protocol == Protocol::Mysql && stream.request_buf.len() >= 4 {
                    let pkt_len = (stream.request_buf[0] as usize)
                        | (stream.request_buf[1] as usize) << 8
                        | (stream.request_buf[2] as usize) << 16;
                    if stream.request_buf.len() >= 4 + pkt_len {
                        stream.request_buf.clear();
                    }
                }
            }
            // If parse_request returns None, keep buffering
        }
        Direction::Response => {
            stream.push_response(payload);
            if handler.needs_response_buffering() && !handler.response_complete(&stream.response_buf) {
                return;
            }
            let buf = &stream.response_buf;
            // If response can't be parsed, it may be incomplete — keep buffering
            let response = match parse_response(protocol, buf) {
                Some(r) => r,
                None => return, // incomplete, wait for more data
            };
            if let Some(pending) = stream.pending_request.take() {
                let response_detail =
                    format_response_detail(protocol, &stream.response_buf).unwrap_or_else(|| response.clone());
                let latency = pending.instant.elapsed();
                let _ = tx.send(ProxyEvent {
                    timestamp: pending.timestamp,
                    component: name.to_string(),
                    protocol,
                    command: pending.command,
                    full_command: pending.full_command,
                    response,
                    response_detail,
                    latency,
                    process: None,
                    src: Some(format!("{}:{}", conn_key.src_ip, conn_key.src_port)),
                    dest: Some(format!("{}:{}", conn_key.dst_ip, conn_key.dst_port)),
                    system: false,
                });
                // Update status for TUI indicator
                if let Ok(mut map) = status.lock() {
                    let entry = map.entry(name.to_string()).or_default();
                    entry.last_active_at = Some(SystemTime::now());
                }
                stream.response_buf.clear();
            } else {
                // Response without pending request (missed the request), skip
                stream.response_buf.clear();
            }
        }
    }
}

/// Parse macOS loopback (BSD null/loopback) header: 4-byte AF family + IP packet
fn parse_loopback(raw: &[u8]) -> Option<etherparse::SlicedPacket<'_>> {
    if raw.len() < 4 {
        return None;
    }
    // BSD loopback: first 4 bytes are address family (AF_INET=2 in host byte order)
    let af = u32::from_ne_bytes([raw[0], raw[1], raw[2], raw[3]]);
    if af != 2 {
        return None; // Only handle IPv4
    }
    etherparse::SlicedPacket::from_ip(&raw[4..]).ok()
}

struct PendingRequest {
    timestamp: SystemTime,
    instant: Instant,
    command: String,
    full_command: String,
}
