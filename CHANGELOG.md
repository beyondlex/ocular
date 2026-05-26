# Changelog

## v0.8.0 (2026-05-26)

### New Features
- **Dashboard group detail page** — press `Space` on a group to open a detail view showing all proxies
  - `j/k` navigate proxies, `n` add, `e` edit, `d` delete with confirmation
  - Changes persist immediately to the group `.toml` file
- **ASCII art logo** — replaces plain "Ocular" title bar on the dashboard

### Improvements
- `ProxyForm::from_entry` supports pre-filled edit forms for existing proxies
- Status bar shows `Space` hint for group detail access

## v0.7.0 (2026-05-25)

### New Features
- **Dashboard** — landing page to select and manage proxy groups before connecting
  - Centered rounded-box UI with group list, scroll support (max 10 visible)
  - `n` create group, `r` rename, `e` edit in $EDITOR, `d` delete with confirm, `/` filter
  - New group wizard: name → add proxies interactively → save
- **Proxy Groups** — organize proxies by environment, stored in `CONFIG_DIR/group/*.toml`
  - Main config `[[proxy]]` entries treated as "default" group
  - Switch groups from main TUI via `Space` → `g`
  - `q` in main TUI returns to dashboard (not quit)
- **Proxy CRUD** — create, edit, delete, inspect proxies from the component pane
  - `n` new proxy form, `e` edit, `d` delete with confirm, `i` info popup
  - Form splits host/port with protocol-aware default port placeholders
  - Port-in-use validation, name uniqueness check
- **Hot-reload proxy connections** — add/remove/modify `[[proxy]]` entries without restarting
  - Existing connections drain naturally when a proxy is removed
- **Component filter** — `/` in component pane for fuzzy search by name/listen/remote

### Improvements
- Component pane: event count colored (green >0, gray 0), address removed (use `i` for details)
- Group name shown in gray after "All" in component pane
- Filter indicators shown in orange (both component and event panes)
- Filter input at bottom of component pane (nvim-style)
- Proxy form fields visually balanced with proper indentation
- Dashboard status bar hints centered
- `Esc` no longer quits dashboard (only `q` / `Ctrl+C`)

### Bug Fixes
- Fix stale events appearing when switching groups (drain rx, filter by active components)
- Fix proxies spawning at startup before user selects a group
- Fix hot-reload watcher triggering before group is loaded
- Fix component pane j/k navigation ignoring filter (cursor on invisible items)

## v0.6.0 (2026-05-24)

### New Features
- **Memcached protocol support** — parse GET/SET/INCR/DELETE/TOUCH/STATS, request/response buffering
- **Kafka protocol support** — parse 30+ ApiKeys (Produce, Fetch, Metadata, JoinGroup, etc.), extract message body from Produce requests
- **Demo mode** (`--demo`) — generates simulated traffic for all protocols, no services needed
- **Follow mode** (`Space f`) — toggle auto-scroll (tail -f), `G` enables, `k`/`gg` disables; FOLLOW indicator in status bar

### Distribution
- **Homebrew** — `brew tap beyondlex/tap && brew install ocular`
- **Cargo** — `cargo install ocular-cli`
- **CI auto-publish** — release workflow publishes to crates.io and updates Homebrew tap automatically

### Improvements
- Install script defaults to `~/.local/bin` (no sudo needed), supports `OCULAR_INSTALL_DIR` env var
- Cleaner `ocular.example.toml` with all config options documented
- README rewrite: origin story, vim-native UX emphasis, 3-line quick start

## v0.5.0 (2026-05-24)

### New Features
- **Fuzzy filter** — event filter now uses fuzzy matching (powered by skim algorithm), matched characters highlighted in yellow/bold in the event list
- **`fuzzy_filter` config** — set `fuzzy_filter = false` in `ocular.toml` to use exact substring matching instead (hot-reloadable)

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
