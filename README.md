# Ocular

A TUI tool for real-time visualization of middleware traffic. See exactly what your services send to Redis, MySQL, RabbitMQ, Elasticsearch — regardless of programming language.

![Rust](https://img.shields.io/badge/rust-stable-orange) ![License](https://img.shields.io/badge/license-MIT-blue)

## How It Works

Ocular runs lightweight TCP proxies between your application and middleware. Traffic flows through transparently while Ocular parses the wire protocol and displays structured events in a terminal dashboard.

```
Your App ──→ Ocular Proxy (16379) ──→ Redis (6379)
                    │
              TUI Dashboard
```

**Zero code changes.** Just point your app's connection to the proxy port.

## Features

- **Real-time event stream** — watch requests/responses as they happen
- **Protocol parsing** — human-readable commands instead of raw bytes
- **Latency tracking** — request→response timing for every operation
- **Filtering** — search by component name or keyword (`/` to activate)
- **Component selection** — focus on a single middleware in the left panel
- **Detail inspector** — select any event to see full payload (scrollable)
- **MySQL ResultSet display** — parsed columns and rows instead of binary
- **Auto SSL stripping** — MySQL connections work without `--ssl-mode=DISABLED`
- **Local timezone** — timestamps match your system clock
- **Vim-style navigation** — `gg`, `G`, `Ngg` line jumps
- **Language agnostic** — works with Java, Rust, Go, Python, anything

## Supported Protocols

| Protocol | Status |
|----------|--------|
| Redis (RESP) | ✅ |
| MySQL | ✅ |
| RabbitMQ (AMQP) | Planned |
| Elasticsearch (HTTP) | Planned |

## Quick Start

```bash
# Build
cargo build --release

# Configure (edit ocular.toml)
cat ocular.toml
```

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

```bash
# Run
./target/release/ocular

# Connect your app to the proxy ports
redis-cli -h 127.0.0.1 -p 16379
mysql -h 127.0.0.1 -P 13306 -u root -p
```

> **Note:** For MySQL, use `-h 127.0.0.1` (not `localhost`) to ensure TCP connection through the proxy.

## Keybindings

| Key | Action |
|-----|--------|
| `j` / `k` | Navigate events / scroll detail |
| `gg` | Jump to first event |
| `G` | Jump to last event |
| `Ngg` | Jump to event N (e.g. `42gg`) |
| `Tab` / `Shift+Tab` | Cycle focus: Components → Events → Detail |
| `/` | Enter filter mode (match component or command) |
| `Enter` | Confirm filter / select component |
| `Esc` | Clear filter or component selection |
| `q` | Quit |

## Architecture

```
crates/
├── ocular/            # Binary entry point, config loading
├── ocular-protocol/   # Wire protocol parsers (RESP, MySQL)
├── ocular-proxy/      # Async TCP proxy with event broadcasting
└── ocular-tui/        # Terminal UI (ratatui)
```

## Logging

Logs are written to `ocular.log` in the working directory (not to stdout, to avoid interfering with the TUI). Control verbosity with `RUST_LOG`:

```bash
RUST_LOG=debug ./target/release/ocular
```

## License

MIT
