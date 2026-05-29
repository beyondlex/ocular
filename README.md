# Ocular

**See what your code actually sends to Redis, MySQL, Postgres, MongoDB, RabbitMQ, Elasticsearch — zero code changes, any language.**

A TUI tool for real-time visualization of middleware traffic. Vim-style keybindings. Protocol-aware parsing. Sub-millisecond latency tracking.

![Rust](https://img.shields.io/badge/rust-stable-orange) ![License](https://img.shields.io/badge/license-MIT-blue)

📖 **[Full Documentation → Wiki](https://github.com/beyondlex/ocular/wiki)**

<img max-width="1370" alt="ocular" src="https://github.com/user-attachments/assets/565c8dbc-b295-4cec-a4a4-7beb6c0ddab9" />

<video src="https://github.com/user-attachments/assets/d4b2803b-2651-4807-9432-3766dd66e9c2" controls="controls" muted="muted" style="max-width: 1370px" autoplay="autoplay" loop="loop">
</video>

## Why Ocular?

- **Not a packet sniffer** — Ocular understands protocols. You see `SET user:1 "hello"`, not hex dumps.
- **Not language-specific** — Works with Java, Rust, Go, Python, Node.js, anything that speaks TCP.
- **Not invasive** — No SDK, no code changes. Point your connection to the proxy port and go.
- **Vim-native UX** — `j/k`, `gg/G`, `/search`, `v`isual select, `y`ank to clipboard. Feels like home.

**Use cases:** Debug N+1 queries, verify cache hits, trace message routing, profile slow queries, audit what your ORM actually sends.

## Quick Start

### Install

```bash
# Homebrew
brew tap beyondlex/tap && brew install ocular

# Cargo
cargo install ocular-cli

# Shell
curl -fsSL https://raw.githubusercontent.com/beyondlex/ocular/main/install.sh | sh
```

Try it instantly — no services needed:

```bash
ocular --demo
```

### With real services

1. Create `ocular.toml`:

```toml
[[proxy]]
name = "redis"
protocol = "redis"
listen = "127.0.0.1:16379"
remote = "127.0.0.1:6379"

[[proxy]]
name = "mysql"
protocol = "mysql"
listen = "127.0.0.1:13306"
remote = "127.0.0.1:3306"
```

2. Run and connect:

```bash
ocular
# Point your app to proxy ports:
redis-cli -h 127.0.0.1 -p 16379
mysql -h 127.0.0.1 -P 13306 -u root -p
```

```
Your App ──→ Ocular Proxy (16379) ──→ Redis (6379)
                    │
              TUI Dashboard
```

> **Note:** For MySQL, use `-h 127.0.0.1` (not `localhost`) to ensure TCP connection through the proxy.

### Passive capture mode (no config changes)

Observe traffic without modifying your application's connection settings. Ocular captures packets directly from the network interface using libpcap.

```toml
[[proxy]]
name = "redis"
protocol = "redis"
mode = "capture"
interface = "lo0"           # macOS: lo0/en0. Linux: lo/eth0
remote = "127.0.0.1:6379"  # The real service address
```

```bash
sudo ocular   # Requires packet capture permissions
```

```
Your App ──→ Redis (6379)    # Direct connection, no changes
                │
         libpcap captures
                │
          TUI Dashboard
```

> Capture mode cannot decrypt SSL/TLS traffic. Use proxy mode for encrypted connections. See [Proxy Mode vs Capture Mode](https://github.com/beyondlex/ocular/wiki/Proxy-Mode-vs-Capture-Mode) for a detailed comparison.

### CLI mode (no TUI, terminal output)

Skip the dashboard and output events directly to the terminal. Ideal for scripting, AI agents, and quick debugging.

```bash
# Proxy mode
ocular proxy mysql 192.168.0.184
# ↳ auto-assigns listen port (3306 + 10000 = 13306)

# Capture mode — passive sniffing, no connection changes
sudo ocular capture mysql 192.168.0.184

# Minimal — defaults to 127.0.0.1 with protocol's default port
ocular proxy redis

# JSON output (one object per line, for programmatic parsing)
ocular proxy mysql --json

# Raw output (no colors, for file redirection)
ocular proxy mysql 192.168.0.184 --raw >> events.log
```

```
11:31:34.444 [mysql] select @@version_comment limit 1 → ResultSet (1 rows, 1 cols) 1.75ms
11:31:34.448 [mysql] SELECT * FROM users LIMIT 5 → ResultSet (5 rows, 5 cols) 3.73ms
```

| Flag | Description |
|------|-------------|
| `--json` | Output as JSON (one object per line) |
| `--raw` | No ANSI colors (auto-enabled when stdout is not a TTY) |
| `--tui`, `-t` | Launch minimal TUI preview (no component pane) |
| `-i`, `--interface` | Network interface for capture mode |
| `-l`, `--listen` | Listen address (default: `0.0.0.0:<remote_port+10000>`) |
| `-c`, `--config` | Path to config file (default: auto-detect) |

See [CLI Reference](https://github.com/beyondlex/ocular/wiki/CLI-Reference) for all options and scripting examples.

## Features

- **Real-time event stream** — watch requests/responses as they happen
- **Protocol parsing** — human-readable commands instead of raw bytes
- **Latency tracking** — request→response timing for every operation
- **Fuzzy filtering** — search by component name or keyword (`/` to activate)
- **Detail inspector** — select any event to see full payload (scrollable)
- **MySQL ResultSet display** — parsed columns and rows instead of binary
- **Auto SSL stripping** — MySQL connections work without `--ssl-mode=DISABLED`
- **Vim-style navigation** — `j/k`, `gg`, `G`, visual select, yank to clipboard
- **Open in $EDITOR** — `e` opens selected commands in vim/nvim
- **Hot-reload config** — change theme, format, filters without restarting
- **Hot-reload proxies** — add/remove/modify `[[proxy]]` entries without restarting
- **Dashboard** — landing page to select and manage proxy groups
- **Proxy Groups** — organize proxies by environment (dev, test, prod)
- **Demo mode** — `--demo` generates simulated traffic for instant preview
- **Event log** — record all events to file for offline analysis
- **Event filtering** — exclude/include rules to hide noisy events

See [Keybindings](https://github.com/beyondlex/ocular/wiki/Keybindings) for the full keyboard shortcut reference.

## Supported Protocols

| Protocol | Proxy Mode | Capture Mode | Notes |
|----------|:----------:|:------------:|-------|
| Redis (RESP) | ✅ | ✅ | Full RESP2/RESP3 |
| MySQL | ✅ | ✅ | Auto SSL stripping, auth handling |
| PostgreSQL | ✅ | ✅ | Simple + extended query protocol |
| RabbitMQ (AMQP) | ✅ | ✅ | AMQP 0-9-1 |
| MongoDB | ✅ | ✅ | OP_MSG; legacy OP_QUERY filtered |
| Memcached | ✅ | ✅ | Text protocol |
| Kafka | ✅ | ✅ | Request/response correlation |
| HTTP / Elasticsearch | ✅ | ✅ | HTTP/1.x parsing |

> Capture mode cannot decrypt SSL/TLS. If your service uses encrypted connections, use proxy mode instead.

## Configuration

Ocular looks for `ocular.toml` in the current directory, then `~/.config/ocular/ocular.toml`. Override with `-c, --config <path>`.

```toml
# Theme: "default", "tokyo-night-storm", "dracula", "solarized-light", "solarized-dark"
theme = "tokyo-night-storm"

# Hide noisy events
[exclude.redis]
patterns = ["PING", "INFO"]

[exclude.mysql]
patterns = ["SELECT 1"]

# Define proxies
[[proxy]]
name = "redis"
protocol = "redis"
listen = "127.0.0.1:16379"
remote = "127.0.0.1:6379"

[[proxy]]
name = "mysql"
protocol = "mysql"
listen = "127.0.0.1:13306"
remote = "127.0.0.1:3306"
```

See [Configuration Reference](https://github.com/beyondlex/ocular/wiki/Configuration-Reference) for all options: event format, proxy groups, event logging, per-proxy filtering, and more.

## Origin Story

I inherited a legacy codebase with no tests and needed to refactor it. The problem: clicking a single button on the frontend could trigger requests to Redis, MySQL, RabbitMQ, and who knows what else — but tracing that through layers of spaghetti code was a nightmare.

I didn't want to spend hours reading tangled code just to understand "what actually happens when I click this button?" I wanted to **see** it — which services get hit, what data flows where, how long each call takes.

So I built Ocular. Point your app's connections through it, click the button, and instantly see every query, every cache write, every message published. No code changes, no breakpoints, no guessing.

## How It Works

Ocular supports two modes:

- **Proxy mode** (default) — lightweight TCP proxies sit between your app and middleware. You point your app to the proxy port.
- **Capture mode** — passive packet capture via libpcap observes traffic on the wire. No connection changes needed, but requires elevated permissions.

In both modes, Ocular parses the wire protocol and displays structured events in a terminal dashboard.

```
crates/
├── ocular/            # Binary entry point, config loading
├── ocular-capture/    # Passive packet capture (libpcap, TCP reassembly)
├── ocular-protocol/   # Wire protocol parsers (RESP, MySQL, PG, AMQP, MongoDB, HTTP)
├── ocular-proxy/      # Async TCP proxy with event broadcasting
└── ocular-tui/        # Terminal UI (ratatui)
```

See [Architecture](https://github.com/beyondlex/ocular/wiki/Architecture) for the detailed data flow and crate design.

## Build from Source

```bash
cargo build --release
./target/release/ocular
```

## License

MIT
