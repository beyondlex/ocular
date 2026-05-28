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

### Install

#### Homebrew

```bash
brew tap beyondlex/tap && brew install ocular
```

#### Cargo

```bash
cargo install ocular-cli
```

#### Shell

```bash
curl -fsSL https://raw.githubusercontent.com/beyondlex/ocular/main/install.sh | sh
```

Try it instantly — no services needed

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

# macOS — grant BPF access (no sudo needed after this, until reboot):
sudo chmod g+r /dev/bpf*

# Linux — grant capture capability (persistent):
sudo setcap cap_net_raw+ep $(which ocular)
```

```
Your App ──→ Redis (6379)    # Direct connection, no changes
                │
         libpcap captures
                │
          TUI Dashboard
```

> **Note:** Capture mode and proxy mode are mutually exclusive per service. Use one or the other.

### CLI mode (no TUI, terminal output)

Skip the dashboard and output events directly to the terminal. Ideal for scripting, AI agents, and quick debugging.

```bash
# Proxy mode — auto-assigns listen port (3306 + 10000 = 13306)
ocular proxy mysql 192.168.0.184
# ↳ equivalent: ocular proxy mysql 192.168.0.184:3306 -l 0.0.0.0:13306
# Starts a proxy on all interfaces port 13306 forwarding to the remote MySQL server.

# Capture mode — passive sniffing, no connection changes
sudo ocular capture mysql 192.168.0.184
# ↳ equivalent: sudo ocular capture mysql 192.168.0.184:3306 -i en0
# Passively captures MySQL traffic to the remote host on the default network interface.
# Requires sudo for BPF (packet capture) permissions on macOS.
# To avoid sudo: sudo chmod g+r /dev/bpf* (one-time setup, persists until reboot).

# Minimal — defaults to 127.0.0.1 with protocol's default port
ocular proxy redis
# ↳ equivalent: ocular proxy redis 127.0.0.1:6379 -l 0.0.0.0:16379
# Proxies local Redis traffic, listening on port 16379.

# JSON output (one object per line, for programmatic parsing)
ocular proxy mysql --json
# ↳ equivalent: ocular proxy mysql 127.0.0.1:3306 --json
# Proxies local MySQL and outputs each event as a JSON object.

# Raw output (no colors, for file redirection)
ocular proxy mysql 192.168.0.184 --raw >> events.log
# ↳ equivalent: ocular proxy mysql 192.168.0.184:3306 --raw >> events.log
# Proxies remote MySQL with plain text output appended to a log file.

# Override interface or listen address
sudo ocular cap mysql 192.168.0.184 -i en0
# ↳ `cap` is short for `capture`. Captures MySQL traffic on the en0 interface.
ocular proxy postgres 10.0.0.5 -l :25432
# ↳ equivalent: ocular proxy postgres 10.0.0.5:5432 -l 0.0.0.0:25432
# Proxies remote Postgres with a custom listen port.
```

```
11:31:34.444 [mysql] select @@version_comment limit 1 → ResultSet (1 rows, 1 cols) 1.75ms
11:31:34.448 [mysql] SELECT * FROM users LIMIT 5 → ResultSet (5 rows, 5 cols) 3.73ms
```

| Option | Description |
|--------|-------------|
| `--json` | Output as JSON (one object per line) |
| `--raw` | No ANSI colors (auto-enabled when stdout is not a TTY) |
| `--color` | Force colored output |
| `--tui`, `-t` | Launch minimal TUI preview (no component pane, full features) |
| `-i`, `--interface` | Network interface for capture mode |
| `-l`, `--listen` | Listen address (default: 0.0.0.0:<remote_port+10000>, `:port` shorthand) |
| `-c`, `--config` | Path to config file (default: auto-detect) |

Default ports: redis=6379, mysql=3306, postgres=5432, amqp=5672, mongodb=27017, http=9200, memcached=11211, kafka=9092

## How It Works

Ocular supports two modes:

- **Proxy mode** (default) — lightweight TCP proxies sit between your app and middleware. You point your app to the proxy port.
- **Capture mode** — passive packet capture via libpcap (macOS/Linux) observes traffic on the wire. No connection changes needed, but requires elevated permissions.

In both modes, Ocular parses the wire protocol and displays structured events in a terminal dashboard.

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
- **Hot-reload proxies** — add/remove/modify `[[proxy]]` entries without restarting
- **Dashboard** — landing page to select and manage proxy groups before connecting
- **Proxy Groups** — organize proxies by environment (dev, test, prod), switch instantly
- **Proxy CRUD** — create, edit, delete, inspect proxies from the component pane (`n/e/d/i`)
- **Component filter** — fuzzy search proxies in the component pane (`/`)
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
| Kafka | ✅ |
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
| `?` | Help popup |
| `q` | Back to dashboard |

### Leader Menu (`Space` + key)

| Key | Action |
|-----|--------|
| `h/j/k/l` | Switch panel focus |
| `c` | Clear all events |
| `f` | Toggle follow (tail -f) |
| `p` | Pause/resume event stream |
| `g` | Switch proxy group |
| `,` | Open config in `$EDITOR` |

### Component Pane

| Key | Action |
|-----|--------|
| `n` | Create new proxy |
| `e` | Edit selected proxy |
| `d` | Delete selected proxy |
| `i` | Inspect proxy details |
| `/` | Filter proxies (fuzzy search) |

### Dashboard

| Key | Action |
|-----|--------|
| `j` / `k` | Navigate groups |
| `Enter` | Load group and connect |
| `n` | Create new group |
| `r` | Rename group |
| `e` | Edit group file in `$EDITOR` |
| `d` | Delete group |
| `/` | Filter groups |
| `q` | Quit |

## Configuration

Ocular looks for `ocular.toml` in the following order:

1. `./ocular.toml` (current directory)
2. `$XDG_CONFIG_HOME/ocular/ocular.toml`
3. `~/.config/ocular/ocular.toml` (via `dirs::config_dir()`)
4. `$HOME/.config/ocular/ocular.toml`
5. `SUDO_USER`'s home `~/.config/ocular/ocular.toml` (when running under sudo)
6. Override with `-c, --config <path>`

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
## Proxy Groups

Organize proxies by environment. Groups are stored as separate files in the config directory:

```
~/.config/ocular/
├── ocular.toml          # Main config (theme, event_format, exclude rules)
└── group/
    ├── dev.toml         # Development proxies
    ├── test.toml        # Test environment
    └── prod.toml        # Production (read-only monitoring)
```

Each group file uses the same `[[proxy]]` format. Proxies defined in `ocular.toml` are treated as the "default" group.

On startup, Ocular shows a **Dashboard** where you select which group to load. You can also switch groups at any time with `Space` → `g`.

Creating a new group from the dashboard (`n`) walks you through naming the group and adding proxies interactively.


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
├── ocular-capture/    # Passive packet capture (libpcap, TCP reassembly)
├── ocular-protocol/   # Wire protocol parsers (RESP, MySQL, PG, AMQP, MongoDB, HTTP)
├── ocular-proxy/      # Async TCP proxy with event broadcasting
└── ocular-tui/        # Terminal UI (ratatui)
```

## License

MIT
