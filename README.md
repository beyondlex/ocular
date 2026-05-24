# Ocular

**See what your code actually sends to Redis, MySQL, Postgres, MongoDB, RabbitMQ, Elasticsearch — zero code changes, any language.**

A TUI tool for real-time visualization of middleware traffic. Vim-style keybindings. Protocol-aware parsing. Sub-millisecond latency tracking.

![Rust](https://img.shields.io/badge/rust-stable-orange) ![License](https://img.shields.io/badge/license-MIT-blue)


<img max-width="1370" alt="ocular" src="https://github.com/user-attachments/assets/565c8dbc-b295-4cec-a4a4-7beb6c0ddab9" />


<video src="https://github.com/user-attachments/assets/d4b2803b-2651-4807-9432-3766dd66e9c2" controls="controls" muted="muted" style="max-width: 1370px" autoplay="autoplay" loop="loop">
</video>

## Why Ocular?

- **Not a packet sniffer** — Ocular understands protocols. You see `SET user:1 "hello"`, not hex dumps.
- **Not language-specific** — Works with Java, Rust, Go, Python, Node.js, anything that speaks TCP.
- **Not invasive** — No SDK, no code changes. Point your connection to the proxy port and go.
- **Vim-native UX** — `j/k`, `gg/G`, `/search`, `v`isual select, `y`ank to clipboard. Feels like home.

**Use cases:** Debug N+1 queries, verify cache hits, trace message routing, profile slow queries, audit what your ORM actually sends.

## Origin Story

I inherited a legacy codebase with no tests and needed to refactor it. The problem: clicking a single button on the frontend could trigger requests to Redis, MySQL, RabbitMQ, and who knows what else — but tracing that through layers of spaghetti code was a nightmare.

I didn't want to spend hours reading tangled code just to understand "what actually happens when I click this button?" I wanted to **see** it — which services get hit, what data flows where, how long each call takes.

So I built Ocular. Point your app's connections through it, click the button, and instantly see every query, every cache write, every message published. No code changes, no breakpoints, no guessing.

## Quick Start

```bash
# Install
brew tap beyondlex/tap && brew install ocular
# or: cargo install ocular-cli
# or: curl -fsSL https://raw.githubusercontent.com/beyondlex/ocular/main/install.sh | sh

# Try it instantly — no services needed
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

## How It Works

Ocular runs lightweight TCP proxies between your application and middleware. Traffic flows through transparently while Ocular parses the wire protocol and displays structured events in a terminal dashboard.

## Features

- **Real-time event stream** — watch requests/responses as they happen
- **Protocol parsing** — human-readable commands instead of raw bytes
- **Latency tracking** — request→response timing for every operation
- **Fuzzy filtering** — search by component name or keyword (`/` to activate)
- **Component selection** — focus on a single middleware in the left panel
- **Detail inspector** — select any event to see full payload (scrollable)
- **MySQL ResultSet display** — parsed columns and rows instead of binary
- **Auto SSL stripping** — MySQL connections work without `--ssl-mode=DISABLED`
- **Local timezone** — timestamps match your system clock
- **Vim-style navigation** — `j/k`, `gg`, `G`, `Ngg` line jumps
- **Visual selection** — `v` to select multiple events, copy or open in editor
- **Yank to clipboard** — `y` copies command/SQL to system clipboard
- **Open in $EDITOR** — `e` opens selected commands in vim/nvim
- **Leader menu** — `Space` opens a command palette
- **Hot-reload config** — change theme, format, filters without restarting
- **Demo mode** — `--demo` generates simulated traffic for instant preview

## Supported Protocols

| Protocol | Status |
|----------|--------|
| Redis (RESP) | ✅ |
| MySQL | ✅ |
| PostgreSQL | ✅ |
| RabbitMQ (AMQP) | ✅ |
| MongoDB | ✅ |
| Memcached | ✅ |
| HTTP / Elasticsearch | ✅ |

## Keybindings

| Key | Action |
|-----|--------|
| `j` / `k` | Navigate events / scroll detail |
| `gg` | Jump to first event |
| `G` | Jump to last event |
| `Ngg` | Jump to event N (e.g. `42gg`) |
| `Tab` / `Shift+Tab` | Cycle focus: Components → Events → Detail |
| `/` | Fuzzy filter (match component or command) |
| `Enter` | Confirm filter / select component |
| `Esc` | Clear filter or component selection |
| `v` | Toggle visual (multi-line) selection |
| `y` | Copy selected command(s) to clipboard |
| `e` | Open selected command(s) in `$EDITOR` |
| `Space` | Open leader menu |
| `Space f` | Toggle follow mode (tail -f) |
| `?` | Help popup |
| `q` | Quit |

## Configuration

Ocular looks for `ocular.toml` in the following order:

1. `./ocular.toml` (current directory)
2. `$XDG_CONFIG_HOME/ocular/ocular.toml`
3. `~/.config/ocular/ocular.toml`

Multiple instances of the same protocol are supported — just use different names and ports:

```toml
[[proxy]]
name = "redis-cache"
protocol = "redis"
listen = "127.0.0.1:16379"
remote = "127.0.0.1:6379"

[[proxy]]
name = "redis-session"
protocol = "redis"
listen = "127.0.0.1:16380"
remote = "127.0.0.1:6380"
```

## Event Filtering (Exclude / Include)

Hide noisy events from the Events panel using `exclude` rules. Use `include` to override excludes and force specific events to remain visible.

```toml
# Global exclude by protocol
[exclude.redis]
patterns = ["PING", "INFO"]

[exclude.mysql]
patterns = ["SELECT 1"]

# Per-proxy exclude (merged with global)
[[proxy]]
name = "mysql-dev"
protocol = "mysql"
listen = "127.0.0.1:13306"
remote = "127.0.0.1:3306"
[proxy.exclude]
patterns = ["^SET NAMES"]
regex = true

# Include overrides exclude
[proxy.include]
patterns = ["PING"]
```

Evaluation order: **include match → show** > **exclude match → hide** > **default → show**

| Field | Default | Description |
|-------|---------|-------------|
| `patterns` | (required) | List of strings to match against the event command |
| `case_sensitive` | `false` | Whether matching is case-sensitive |
| `regex` | `false` | Treat patterns as regular expressions |

## Event Line Format

Customize how each event line is displayed using a template string:

```toml
event_format = "%{5}index %time [%{-12}component] %command (%latency)"
```

| Field | Content |
|-------|---------|
| `index` | Line number |
| `time` | Timestamp (local timezone) |
| `component` | Component name |
| `command` | Event command/SQL |
| `latency` | Request→response duration |
| `src` | Source address (ip:port) |
| `dest` | Destination address (ip:port) |

Use `%{N}field` for fixed width (positive = right-aligned, negative = left-aligned). Supports hot-reload.

## Event Log

Record all proxy events to `events.log` for offline analysis:

```toml
[event_log]
enabled = true
include_response = true
components = ["redis-cache", "mysql"]
```

Output:
```
21:08:43.123 [redis-cache] SET user:1 "hello" (0.45ms) -> OK
21:08:43.456 [mysql] SELECT * FROM users (1.23ms) -> ResultSet (19 rows, 3 cols)
```

## Build from Source

```bash
cargo build --release
./target/release/ocular
```

## Architecture

```
crates/
├── ocular/            # Binary entry point, config loading
├── ocular-protocol/   # Wire protocol parsers (RESP, MySQL, PG, AMQP, MongoDB, HTTP)
├── ocular-proxy/      # Async TCP proxy with event broadcasting
└── ocular-tui/        # Terminal UI (ratatui)
```

## License

MIT
