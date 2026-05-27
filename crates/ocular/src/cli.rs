use anyhow::{bail, Result};
use chrono::{DateTime, Local};
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use ocular_protocol::{Protocol, ProxyEvent, StatusMap};
use std::io::{IsTerminal, Write};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputMode {
    Color,
    Raw,
    Json,
}

#[derive(Debug)]
pub enum CliSubcommand {
    Capture,
    Proxy,
}

#[derive(Debug)]
pub struct CliArgs {
    pub subcmd: CliSubcommand,
    pub protocol: Protocol,
    pub remote: String,
    pub output: OutputMode,
    pub interface: Option<String>,
    pub listen: Option<String>,
    pub tui: bool,
}

pub fn parse_cli_args(args: &[String]) -> Result<CliArgs> {
    let subcmd = match args[1].as_str() {
        "capture" | "cap" => CliSubcommand::Capture,
        "proxy" => CliSubcommand::Proxy,
        _ => bail!("unknown subcommand: {}", args[1]),
    };

    if args.len() < 3 {
        print_usage(&args[1]);
        bail!("missing required arguments");
    }

    let protocol = Protocol::parse(&args[2])
        .ok_or_else(|| anyhow::anyhow!("unknown protocol: {}", args[2]))?;

    // args[3] is optional host[:port], but could also be a flag (starts with -)
    let (remote, extra_start) = if args.len() > 3 && !args[3].starts_with('-') {
        let r = if args[3].contains(':') {
            args[3].clone()
        } else {
            format!("{}:{}", args[3], default_port(protocol))
        };
        (r, 4)
    } else {
        (format!("127.0.0.1:{}", default_port(protocol)), 3)
    };

    let mut output = if std::io::stdout().is_terminal() {
        OutputMode::Color
    } else {
        OutputMode::Raw
    };
    let mut interface = None;
    let mut listen = None;
    let mut tui = false;

    let mut i = extra_start;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => output = OutputMode::Json,
            "--raw" => output = OutputMode::Raw,
            "--color" => output = OutputMode::Color,
            "--tui" | "-t" => tui = true,
            "-i" | "--interface" => {
                i += 1;
                interface = Some(args.get(i).cloned().ok_or_else(|| anyhow::anyhow!("--interface requires a value"))?);
            }
            "-l" | "--listen" => {
                i += 1;
                listen = Some(args.get(i).cloned().ok_or_else(|| anyhow::anyhow!("--listen requires a value"))?);
            }
            other => bail!("unknown option: {}", other),
        }
        i += 1;
    }

    Ok(CliArgs { subcmd, protocol, remote, output, interface, listen, tui })
}

fn print_usage(subcmd: &str) {
    eprintln!("Usage: ocular {} <protocol> <host[:port]> [options]", subcmd);
    eprintln!();
    eprintln!("Protocols: redis, mysql, postgres, amqp, mongodb, http, memcached, kafka");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --json              Output as JSON (one object per line)");
    eprintln!("  --raw               Output without colors");
    eprintln!("  --color             Force colored output");
    eprintln!("  --tui, -t           Launch minimal TUI preview");
    eprintln!("  -i, --interface     Network interface (capture mode)");
    eprintln!("  -l, --listen        Listen address (proxy mode, default: auto)");
    eprintln!();
    eprintln!("Port defaults: redis=6379, mysql=3306, postgres=5432, amqp=5672,");
    eprintln!("              mongodb=27017, http=9200, memcached=11211, kafka=9092");
}

fn default_port(protocol: Protocol) -> u16 {
    ocular_protocol::get_handler(protocol).default_port()
}

fn detect_interface(remote: &str) -> String {
    let host = remote.split(':').next().unwrap_or("");
    let is_local = host == "127.0.0.1" || host == "localhost" || host.starts_with("127.");
    if is_local {
        if cfg!(target_os = "macos") { "lo0" } else { "lo" }.to_string()
    } else if cfg!(target_os = "macos") {
        "en0".to_string()
    } else {
        // Linux: parse default route to find interface
        std::process::Command::new("ip")
            .args(["route", "show", "default"])
            .output()
            .ok()
            .and_then(|o| {
                let s = String::from_utf8_lossy(&o.stdout);
                // "default via x.x.x.x dev eth0 ..."
                s.split_whitespace().skip_while(|w| *w != "dev").nth(1).map(String::from)
            })
            .unwrap_or_else(|| "eth0".to_string())
    }
}

fn resolve_listen_addr(remote: &str) -> Result<String> {
    let port: u16 = remote.rsplit(':').next()
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("cannot parse port from remote: {}", remote))?;
    let preferred = port.wrapping_add(10000);
    let addr = format!("127.0.0.1:{}", preferred);
    // Try binding to check availability
    match std::net::TcpListener::bind(&addr) {
        Ok(_) => Ok(addr),
        Err(_) => {
            // Fallback: OS-assigned port
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            let actual = listener.local_addr()?;
            Ok(actual.to_string())
        }
    }
}

fn format_event_color(ev: &ProxyEvent, out: &mut impl Write) {
    let ts: DateTime<Local> = ev.timestamp.into();
    let latency_ms = ev.latency.as_secs_f64() * 1000.0;
    let resp_color = if ev.response.starts_with("ERR") || ev.response.starts_with("Error") {
        Color::Red
    } else {
        Color::Green
    };
    let _ = write!(out, "{}{}{}",
        SetForegroundColor(Color::DarkGrey),
        ts.format("%H:%M:%S%.3f"),
        ResetColor,
    );
    let _ = write!(out, " {}[{}]{}",
        SetForegroundColor(Color::Cyan),
        ev.component,
        ResetColor,
    );
    let _ = write!(out, " {}", ev.full_command.replace('\n', " "));
    if !ev.response.is_empty() {
        let _ = write!(out, " → {}{}{}",
            SetForegroundColor(resp_color),
            ev.response.replace('\n', " "),
            ResetColor,
        );
    }
    let _ = writeln!(out, " {}{:.2}ms{}",
        SetForegroundColor(Color::Yellow),
        latency_ms,
        ResetColor,
    );
}

fn format_event_raw(ev: &ProxyEvent, out: &mut impl Write) {
    let ts: DateTime<Local> = ev.timestamp.into();
    let latency_ms = ev.latency.as_secs_f64() * 1000.0;
    let cmd = ev.full_command.replace('\n', " ");
    if ev.response.is_empty() {
        let _ = writeln!(out, "{} [{}] {} {:.2}ms",
            ts.format("%H:%M:%S%.3f"), ev.component, cmd, latency_ms);
    } else {
        let _ = writeln!(out, "{} [{}] {} → {} {:.2}ms",
            ts.format("%H:%M:%S%.3f"), ev.component, cmd,
            ev.response.replace('\n', " "), latency_ms);
    }
}

fn format_event_json(ev: &ProxyEvent, out: &mut impl Write) {
    let ts: DateTime<Local> = ev.timestamp.into();
    let latency_ms = ev.latency.as_secs_f64() * 1000.0;
    // Manual JSON to avoid adding serde_json dependency
    let _ = writeln!(out,
        r#"{{"timestamp":"{}","protocol":"{:?}","command":"{}","response":"{}","latency_ms":{:.2},"src":{},"dest":{}}}"#,
        ts.format("%H:%M:%S%.3f"),
        ev.protocol,
        ev.full_command.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"),
        ev.response.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"),
        latency_ms,
        ev.src.as_ref().map(|s| format!("\"{}\"", s)).unwrap_or_else(|| "null".to_string()),
        ev.dest.as_ref().map(|s| format!("\"{}\"", s)).unwrap_or_else(|| "null".to_string()),
    );
}

pub async fn run_cli(args: CliArgs) -> Result<()> {
    // Init tracing to stderr if RUST_LOG is set
    if std::env::var("RUST_LOG").is_ok() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .try_init();
    }
    let (tx, _) = broadcast::channel::<ProxyEvent>(4096);
    let status: StatusMap = Arc::new(Mutex::new(std::collections::HashMap::new()));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let name = format!("{:?}", args.protocol).to_lowercase();

    match args.subcmd {
        CliSubcommand::Capture => {
            let interface = args.interface.clone().unwrap_or_else(|| detect_interface(&args.remote));
            if !args.tui {
                eprintln!("Capturing {:?} on {} (filter: tcp port {})",
                    args.protocol, interface,
                    args.remote.rsplit(':').next().unwrap_or("?"));
            }

            #[cfg(feature = "capture")]
            {
                let cfg = ocular_capture::CaptureConfig {
                    name: name.clone(),
                    protocol: args.protocol,
                    interface,
                    remote: args.remote,
                };
                let tx_clone = tx.clone();
                let status_clone = status.clone();
                tokio::spawn(async move {
                    if let Err(e) = ocular_capture::run_capture(cfg, tx_clone, shutdown_rx, status_clone).await {
                        eprintln!("capture error: {}", e);
                    }
                });
            }
            #[cfg(not(feature = "capture"))]
            bail!("capture mode not supported (compiled without capture feature)");
        }
        CliSubcommand::Proxy => {
            let listen = match args.listen {
                Some(ref l) => l.clone(),
                None => resolve_listen_addr(&args.remote)?,
            };
            if !args.tui {
                eprintln!("Proxying {:?} on {} → {}", args.protocol, listen, args.remote);
            }

            let listen_clone = listen.clone();
            let remote = args.remote.clone();
            let tx_clone = tx.clone();
            let status_clone = status.clone();
            let name_clone = name.clone();
            let protocol = args.protocol;
            tokio::spawn(async move {
                if let Err(e) = ocular_proxy::run_proxy(
                    listen_clone, remote, name_clone, protocol, tx_clone, shutdown_rx, status_clone,
                ).await {
                    eprintln!("proxy error: {}", e);
                }
            });
        }
    }

    if args.tui {
        let rx = tx.subscribe();
        let component = ocular_tui::ComponentInfo {
            name: name.clone(),
            listen: String::new(),
            exclude: None,
            include: None,
        };
        let theme = ocular_tui::Theme::by_name("default");
        let config_path = std::path::PathBuf::from("ocular.toml");
        let result = ocular_tui::run(
            rx, vec![component], theme, config_path.clone(),
            None, true, false, None, None, None, None, config_path, status, true,
        ).await;
        let _ = shutdown_tx.send(true);
        return result;
    }

    let mut rx = tx.subscribe();
    let mut stdout = std::io::stdout().lock();

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = async {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        if ev.system { continue; }
                        match args.output {
                            OutputMode::Color => format_event_color(&ev, &mut stdout),
                            OutputMode::Raw => format_event_raw(&ev, &mut stdout),
                            OutputMode::Json => format_event_json(&ev, &mut stdout),
                        }
                        let _ = stdout.flush();
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        } => {}
    }

    let _ = shutdown_tx.send(true);
    eprintln!("\nShutting down.");
    Ok(())
}
