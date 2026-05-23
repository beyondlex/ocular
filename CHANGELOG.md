# Changelog

## v0.2.0 (2026-05-23)

### New Features
- **MongoDB protocol support** — parse find/insert/update/delete, mongosh-style commands in Detail
- **HTTP protocol support** — generic HTTP/1.1 with JSON highlighting, curl copy (y key)
- **TLS outbound** — proxy to HTTPS targets (`remote = "https://..."`)
- **Event replay** — copy HTTP events as executable curl commands
- **Latency threshold** — highlight slow events in red (`latency_threshold_ms` config, hot-reloadable)
- **Pause buffers events** — resume flushes all buffered events
- **src/dest addresses** — show client and remote address in events and Detail
- **Help popup** (`?`) — all keybindings in one place
- **Quit confirm** (`q` → y/n) — configurable via `quit_confirm`
- **Leader menu config** — `leader_menu = false` to hide popup
- **h/l panel navigation** — direct left/right panel switching
- **Mode indicator** — NORMAL/VISUAL/LEADER in status bar
- **Absolute event index** — filter doesn't reset line numbers

### Improvements
- **Detail pane redesign** — compact layout with sticky metadata footer
- **AMQP fixes** — correct body extraction, Deliver direction, exchange in summary
- **JSON syntax highlighting** — in HTTP response body
- **Event log** — includes src/dest, newlines sanitized
- **ProtocolHandler trait** — extensible architecture for adding protocols

### Testing
- 28 unit tests covering all protocol parsers
- Docker Compose testing suite (Redis, MySQL, PostgreSQL, RabbitMQ, MongoDB, Elasticsearch, HTTPS)
- CI workflow (build + test + clippy)

## v0.1.0 (2026-05-21)

- Initial release
- Redis, MySQL, PostgreSQL, RabbitMQ support
- TUI with event stream, filtering, detail panel
- Event format customization
- Exclude/include patterns
