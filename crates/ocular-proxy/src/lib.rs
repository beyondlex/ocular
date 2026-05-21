use anyhow::Result;
use ocular_protocol::{Direction, parse_resp};
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
    tx: broadcast::Sender<ProxyEvent>,
) -> Result<()> {
    let listener = TcpListener::bind(&listen_addr).await?;
    info!(component = %name, listen = %listen_addr, remote = %remote_addr, "proxy listening");

    loop {
        let (client, peer) = listener.accept().await?;
        debug!(component = %name, peer = %peer, "new client connection");
        let remote = remote_addr.clone();
        let name = name.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(client, &remote, &name, &tx).await {
                warn!(component = %name, remote = %remote, error = %e, "connection ended with error");
            }
        });
    }
}

async fn handle_conn(
    mut client: TcpStream,
    remote_addr: &str,
    name: &str,
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
            let err_msg = format!("-ERR ocular proxy: cannot reach {} ({})\r\n", remote_addr, e);
            let _ = client.write_all(err_msg.as_bytes()).await;
            return Err(e.into());
        }
    };

    let (mut cr, mut cw) = client.split();
    let (mut sr, mut sw) = server.split();

    // 共享的最近 request 时间戳，用于计算 response 耗时
    let last_req_time: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));

    let name_req = name.to_string();
    let name_resp = name.to_string();
    let tx_req = tx.clone();
    let tx_resp = tx.clone();
    let req_time_w = last_req_time.clone();
    let req_time_r = last_req_time;

    let client_to_server = async move {
        let mut buf = [0u8; 4096];
        loop {
            let n = cr.read(&mut buf).await?;
            if n == 0 { break; }
            let data = &buf[..n];
            if let Ok(Some((val, _))) = parse_resp(data) {
                let now = Instant::now();
                *req_time_w.lock().await = Some(now);
                let summary = val.to_command_string();
                debug!(component = %name_req, direction = "request", %summary);
                let _ = tx_req.send(ProxyEvent {
                    timestamp: SystemTime::now(),
                    component: name_req.clone(),
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
        let mut buf = [0u8; 4096];
        loop {
            let n = sr.read(&mut buf).await?;
            if n == 0 { break; }
            let data = &buf[..n];
            if let Ok(Some((val, _))) = parse_resp(data) {
                let latency = req_time_r.lock().await.take().map(|t| t.elapsed());
                let summary = val.to_command_string();
                debug!(component = %name_resp, direction = "response", %summary, ?latency);
                let _ = tx_resp.send(ProxyEvent {
                    timestamp: SystemTime::now(),
                    component: name_resp.clone(),
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
