# Changelog

## v0.4.0 (2026-05-23)

### New Features
- **MongoDB protocol support** — parse find/insert/update/delete, mongosh-style commands in Detail
- **HTTP protocol support** — generic HTTP/1.1 with JSON highlighting, curl copy (y key)
- **TLS outbound** — proxy to HTTPS targets (`remote = "https://..."`)
- **Latency threshold** — highlight slow events in red (`latency_threshold_ms` config, hot-reloadable)
- **Pause buffers events** — resume flushes all buffered events
- **Absolute event index** — filter doesn't reset line numbers
- **ProtocolHandler trait** — extensible architecture for adding protocols

### Testing & CI
- 28 unit tests covering all protocol parsers
- Docker Compose testing suite (Redis, MySQL, PostgreSQL, RabbitMQ, MongoDB, Elasticsearch, HTTPS)
- CI workflow (build + test + clippy)
- CONTRIBUTING.md, issue templates

## v0.3.0 (2026-05-22)

### New Features
- **src/dest addresses** — show client and remote address in events and Detail
- **Help popup** (`?`) — all keybindings in one place
- **Quit confirm** (`q` → y/n) — configurable via `quit_confirm`
- **Leader menu config** — `leader_menu = false` to hide popup
- **h/l panel navigation** — direct left/right panel switching
- **Mode indicator** — NORMAL/VISUAL/LEADER in status bar

### Improvements
- **Detail pane redesign** — compact layout with sticky metadata footer
- **AMQP fixes** — correct body extraction, Deliver direction, exchange in summary
- **Event log** — includes src/dest, newlines sanitized

## v0.2.0 (2026-05-21)

- PostgreSQL protocol support with SSL negotiation
- Ctrl+C force quit
- Event count per component
- UI polish: styled key hints, leader menu edit config

## v0.1.2 (2026-05-21)

- Event format customization (`event_format` config)
- Vim-style navigation (gg, G, Ngg)
- Visual selection, yank, open in editor

## v0.1.1 (2026-05-21)

- Exclude/include pattern filtering
- Local timezone timestamps

## v0.1.0 (2026-05-21)

- Initial release
- Redis, MySQL, RabbitMQ (AMQP) support
- TUI with event stream, filtering, detail panel
