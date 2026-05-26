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
) -> Result<()> {
    let remote_addr: std::net::SocketAddr = config
        .remote
        .parse()
        .with_context(|| format!("invalid remote address: {}", config.remote))?;
    let port = remote_addr.port();

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

fn process_packet(
    raw: &[u8],
    remote_addr: std::net::SocketAddr,
    name: &str,
    protocol: Protocol,
    handler: &'static dyn ocular_protocol::ProtocolHandler,
    streams: &mut HashMap<ConnKey, TcpStreamState>,
    tx: &broadcast::Sender<ProxyEvent>,
    datalink: pcap::Linktype,
) {
    use etherparse::SlicedPacket;

    // DLT_NULL (BSD loopback) = 0, DLT_EN10MB (Ethernet) = 1
    let packet = if datalink == pcap::Linktype(0) {
        match parse_loopback(raw) {
            Some(p) => p,
            None => return,
        }
    } else {
        match SlicedPacket::from_ethernet(raw) {
            Ok(p) => p,
            Err(_) => return,
        }
    };

    let (src_ip, dst_ip) = match packet.net {
        Some(etherparse::NetSlice::Ipv4(ref h)) => {
            (h.header().source_addr(), h.header().destination_addr())
        }
        _ => return,
    };

    let (src_port, dst_port, payload) = match packet.transport {
        Some(etherparse::TransportSlice::Tcp(ref tcp)) => {
            (tcp.source_port(), tcp.destination_port(), tcp.payload())
        }
        _ => return,
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

    // Determine direction: to remote = request, from remote = response
    let (direction, conn_key) = if std::net::IpAddr::from(dst_ip) == remote_ip && dst_port == remote_port {
        (Direction::Request, ConnKey::new(src_ip.into(), src_port, dst_ip.into(), dst_port))
    } else if std::net::IpAddr::from(src_ip) == remote_ip && src_port == remote_port {
        (Direction::Response, ConnKey::new(dst_ip.into(), dst_port, src_ip.into(), src_port))
    } else {
        return; // not relevant
    };

    let stream = streams.entry(conn_key).or_insert_with(TcpStreamState::new);

    match direction {
        Direction::Request => {
            stream.push_request(payload);
            // Try to parse complete request
            if handler.needs_request_buffering() {
                if !handler.request_complete(&stream.request_buf) {
                    return;
                }
            }
            let buf = &stream.request_buf;
            if let Some(command) = parse_request(protocol, buf) {
                let full_command =
                    extract_full_command(protocol, buf).unwrap_or_else(|| command.clone());
                stream.pending_request = Some(PendingRequest {
                    timestamp: SystemTime::now(),
                    instant: Instant::now(),
                    command,
                    full_command,
                });
                stream.request_buf.clear();
            }
            // If parse_request returns None, keep buffering
        }
        Direction::Response => {
            stream.push_response(payload);
            if handler.needs_response_buffering() {
                if !handler.response_complete(&stream.response_buf) {
                    return;
                }
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
