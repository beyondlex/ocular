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
- **Detail inspector** — select any event to see full payload
- **Local timezone** — timestamps match your system clock
- **Language agnostic** — works with Java, Rust, Go, Python, anything

## Supported Protocols

| Protocol | Status |
|----------|--------|
| Redis (RESP) | ✅ |
| MySQL | Planned |
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
```

```bash
# Run
./target/release/ocular

# Connect your app to localhost:16379 instead of 6379
redis-cli -p 16379
```

## Keybindings

| Key | Action |
|-----|--------|
| `j` / `k` | Navigate events / scroll detail |
| `Tab` | Switch focus between Events and Detail panels |
| `/` | Enter filter mode |
| `Enter` | Confirm filter |
| `Esc` | Clear filter |
| `q` | Quit |

## Architecture

```
crates/
├── ocular/            # Binary entry point, config loading
├── ocular-protocol/   # Wire protocol parsers (RESP, ...)
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
