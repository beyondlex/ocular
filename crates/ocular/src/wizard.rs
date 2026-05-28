use anyhow::{bail, Result};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, ClearType},
};
use std::io::{stdout, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

// ─── Protocol table ─────────────────────────────────────────────────────────

const SELECTION_HINT: &str = "\u{2191}\u{2193} navigate  Enter confirm  Esc back  Ctrl+C quit";
const INPUT_HINT: &str = "Enter confirm  Esc back";

const PROTOCOLS: &[(&str, &str, u16)] = &[
    ("redis",         "Redis",         6379),
    ("mysql",         "MySQL",         3306),
    ("postgres",      "PostgreSQL",    5432),
    ("mongodb",       "MongoDB",       27017),
    ("rabbitmq",      "RabbitMQ",      5672),
    ("kafka",         "Kafka",         9092),
    ("elasticsearch", "Elasticsearch", 9200),
    ("memcached",     "Memcached",     11211),
];

// ─── Types ──────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct WizardProxy {
    name: String,
    protocol: String,
    mode: Mode,
    host: String,
    port: u16,
    listen_addr: String,
    interface: String,
    reachable: Option<bool>,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Proxy,
    Capture,
}

// ─── Terminal helpers ───────────────────────────────────────────────────────

struct Term;

impl Term {
    fn color(c: Color) {
        let _ = execute!(stdout(), SetForegroundColor(c));
    }

    fn reset() {
        let _ = execute!(stdout(), ResetColor);
    }

    fn bold_on() {
        let _ = execute!(stdout(), crossterm::style::SetAttribute(crossterm::style::Attribute::Bold));
    }

    fn bold_off() {
        let _ = execute!(stdout(), crossterm::style::SetAttribute(crossterm::style::Attribute::Reset));
    }

    fn clear_line() {
        let _ = execute!(stdout(), terminal::Clear(ClearType::CurrentLine), cursor::MoveToColumn(0));
    }

    fn clear_screen() {
        let _ = execute!(
            stdout(),
            terminal::Clear(ClearType::All),
            cursor::MoveTo(0, 0)
        );
    }

    fn print(s: &str) {
        let _ = execute!(stdout(), Print(s));
    }

    fn println(s: &str) {
        let _ = execute!(stdout(), Print(format!("{}\r\n", s)));
    }

    fn flush() {
        let _ = stdout().flush();
    }
}

/// Read arrow-key selection from a list of options.
/// Returns `Ok(Some(index))` on Enter, `Ok(None)` on Esc (go back).
/// Ctrl+C returns Err.
/// Hint is rendered below the options (static, not re-rendered on each keypress).
fn read_selection(options: &[(&str, &str)], hint: &str) -> Result<Option<usize>> {
    terminal::enable_raw_mode()?;
    let _ = execute!(stdout(), cursor::Hide);
    let mut selected: usize = 0;
    let n = options.len();
    let hint_lines: u16 = if hint.is_empty() { 0 } else { 2 }; // blank + hint

    /// Render all options starting at the current cursor line.
    /// After rendering, cursor is N lines below the first option.
    fn render_options(options: &[(&str, &str)], selected: usize) {
        for (i, (label, desc)) in options.iter().enumerate() {
            Term::clear_line();
            if i == selected {
                Term::color(Color::Cyan);
                Term::bold_on();
                Term::print("  \u{276f} ");
                Term::print(label);
                Term::bold_off();
                if !desc.is_empty() {
                    Term::color(Color::DarkGrey);
                    Term::print(&format!("  \u{2014} {}", desc));
                }
                Term::reset();
            } else {
                Term::print(&format!("    {}", label));
                if !desc.is_empty() {
                    Term::color(Color::DarkGrey);
                    Term::print(&format!("  \u{2014} {}", desc));
                    Term::reset();
                }
            }
            Term::print("\r\n");
        }
    }

    fn render_hint(hint: &str) {
        if !hint.is_empty() {
            Term::println("");
            Term::color(Color::DarkGrey);
            Term::println(&format!("    {}", hint));
            Term::reset();
        }
    }

    // Initial render: options + hint
    render_options(options, selected);
    render_hint(hint);
    Term::flush();

    loop {
        // Cursor is N+hint_lines below first option line — move back to top
        let _ = execute!(stdout(), cursor::MoveUp(n as u16 + hint_lines), cursor::MoveToColumn(0));

        if let Event::Key(KeyEvent { code, modifiers, .. }) = event::read()? {
            if matches!(code, KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL)) {
                // Clean up: clear all option lines + hint, cursor ends after area
                for _ in 0..n {
                    Term::clear_line();
                    let _ = execute!(stdout(), cursor::MoveToNextLine(1));
                }
                for _ in 0..hint_lines {
                    Term::clear_line();
                    let _ = execute!(stdout(), cursor::MoveToNextLine(1));
                }
                let _ = execute!(stdout(), cursor::Show);
                terminal::disable_raw_mode()?;
                bail!("cancelled");
            }
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected.saturating_sub(1);
                    render_options(options, selected);
                    render_hint(hint);
                    Term::flush();
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1).min(n - 1);
                    render_options(options, selected);
                    render_hint(hint);
                    Term::flush();
                }
                KeyCode::Enter => {
                    render_options(options, selected);
                    render_hint(hint);
                    let _ = execute!(stdout(), cursor::MoveUp(n as u16 + hint_lines), cursor::MoveToColumn(0));
                    // Skip to selected line
                    for _ in 0..selected {
                        let _ = execute!(stdout(), cursor::MoveToNextLine(1));
                    }
                    // Print selected option on this line
                    Term::clear_line();
                    Term::color(Color::Cyan);
                    Term::bold_on();
                    Term::print("  \u{276f} ");
                    Term::print(options[selected].0);
                    Term::bold_off();
                    if !options[selected].1.is_empty() {
                        Term::color(Color::DarkGrey);
                        Term::print(&format!("  \u{2014} {}", options[selected].1));
                    }
                    Term::reset();
                    Term::print("\r\n");
                    // Clear remaining option lines below selected + hint lines
                    let remaining = n - 1 - selected;
                    for _ in 0..remaining {
                        Term::clear_line();
                        let _ = execute!(stdout(), cursor::MoveToNextLine(1));
                    }
                    for _ in 0..hint_lines {
                        Term::clear_line();
                        let _ = execute!(stdout(), cursor::MoveToNextLine(1));
                    }
                    let _ = execute!(stdout(), cursor::Show);
                    terminal::disable_raw_mode()?;
                    return Ok(Some(selected));
                }
                KeyCode::Esc => {
                    // Cancel: clear all option lines + hint, cursor ends after area
                    for _ in 0..n {
                        Term::clear_line();
                        let _ = execute!(stdout(), cursor::MoveToNextLine(1));
                    }
                    for _ in 0..hint_lines {
                        Term::clear_line();
                        let _ = execute!(stdout(), cursor::MoveToNextLine(1));
                    }
                    let _ = execute!(stdout(), cursor::Show);
                    terminal::disable_raw_mode()?;
                    return Ok(None);
                }
                _ => {
                    // Unknown key — just re-render (already rendered at top of loop)
                }
            }
        }
    }
}

/// Read a line of text with a default value.
/// Returns `Ok(Some(text))` on Enter, `Ok(None)` on Esc (go back).
/// Ctrl+C returns Err.
/// Hint is rendered below the input line (static).
fn read_text(default: &str, hint: &str) -> Result<Option<String>> {
    terminal::enable_raw_mode()?;
    let _ = execute!(stdout(), cursor::Hide);
    let mut input = String::new();
    let hint_lines: u16 = if hint.is_empty() { 0 } else { 2 }; // blank + hint

    let render_input = |input: &str| {
        Term::clear_line();
        Term::color(Color::Cyan);
        Term::print("  \u{276f} ");
        Term::reset();
        if input.is_empty() {
            Term::color(Color::DarkGrey);
            Term::print(default);
            Term::reset();
        } else {
            Term::print(input);
        }
        Term::print(" \u{258c}"); // cursor block
        Term::flush();
    };

    // Initial render: input line + hint below
    render_input(&input);
    if !hint.is_empty() {
        Term::println("");
        Term::color(Color::DarkGrey);
        Term::println(&format!("    {}", hint));
        Term::reset();
        // Move cursor back to input line
        let _ = execute!(stdout(), cursor::MoveUp(hint_lines), cursor::MoveToColumn(0));
    }

    loop {
        if let Event::Key(KeyEvent { code, modifiers, .. }) = event::read()? {
            if matches!(code, KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL)) {
                let _ = execute!(stdout(), cursor::Show);
                terminal::disable_raw_mode()?;
                Term::clear_line();
                for _ in 0..hint_lines {
                    let _ = execute!(stdout(), cursor::MoveToNextLine(1));
                    Term::clear_line();
                }
                Term::println("");
                bail!("cancelled");
            }
            match code {
                KeyCode::Enter => {
                    Term::clear_line();
                    // Clear hint lines
                    for _ in 0..hint_lines {
                        let _ = execute!(stdout(), cursor::MoveToNextLine(1));
                        Term::clear_line();
                    }
                    let _ = execute!(stdout(), cursor::Show);
                    terminal::disable_raw_mode()?;
                    let result = if input.is_empty() { default.to_string() } else { input };
                    return Ok(Some(result));
                }
                KeyCode::Esc => {
                    Term::clear_line();
                    for _ in 0..hint_lines {
                        let _ = execute!(stdout(), cursor::MoveToNextLine(1));
                        Term::clear_line();
                    }
                    let _ = execute!(stdout(), cursor::Show);
                    terminal::disable_raw_mode()?;
                    return Ok(None);
                }
                KeyCode::Backspace => {
                    input.pop();
                    render_input(&input);
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    render_input(&input);
                }
                _ => {}
            }
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn detect_interface(remote_host: &str) -> String {
    let is_local = remote_host == "127.0.0.1" || remote_host == "localhost" || remote_host.starts_with("127.");
    if is_local {
        if cfg!(target_os = "macos") { "lo0" } else { "lo" }.to_string()
    } else if cfg!(target_os = "macos") {
        "en0".to_string()
    } else {
        std::process::Command::new("ip")
            .args(["route", "show", "default"])
            .output()
            .ok()
            .and_then(|o| {
                let s = String::from_utf8_lossy(&o.stdout);
                s.split_whitespace().skip_while(|w| *w != "dev").nth(1).map(String::from)
            })
            .unwrap_or_else(|| "eth0".to_string())
    }
}

fn auto_assign_listen_port(port: u16) -> String {
    let preferred = port.saturating_add(10000);
    let addr = format!("127.0.0.1:{}", preferred);
    if std::net::TcpListener::bind(&addr).is_ok() {
        return addr;
    }
    // Scan a few more
    for offset in 1..100u16 {
        let try_port = preferred + offset;
        let addr = format!("127.0.0.1:{}", try_port);
        if std::net::TcpListener::bind(&addr).is_ok() {
            return addr;
        }
    }
    // Fallback: OS-assigned
    if let Ok(listener) = std::net::TcpListener::bind("127.0.0.1:0") {
        if let Ok(addr) = listener.local_addr() {
            return addr.to_string();
        }
    }
    format!("127.0.0.1:{}", preferred)
}

async fn check_connectivity(host: &str, port: u16) -> Option<Duration> {
    let addr = format!("{}:{}", host, port);
    let start = Instant::now();
    match tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::TcpStream::connect(&addr),
    )
    .await
    {
        Ok(Ok(_)) => Some(start.elapsed()),
        _ => None,
    }
}

fn connectivity_hint(protocol: &str, host: &str, port: u16) -> String {
    match protocol {
        "redis" => format!("redis-cli -h {} -p {} ping", host, port),
        "mysql" => format!("mysql -h {} -P {} -u root -p", host, port),
        "postgres" => format!("psql -h {} -p {} -U postgres", host, port),
        "mongodb" => format!("mongosh --host {} --port {}", host, port),
        "rabbitmq" => format!("rabbitmqctl -n rabbit@{} status", host),
        "kafka" => format!("nc -zv {} {}", host, port),
        "elasticsearch" => format!("curl -s http://{}:{}/_cluster/health", host, port),
        "memcached" => format!("nc -zv {} {}", host, port),
        _ => format!("nc -zv {} {}", host, port),
    }
}

fn parse_address(input: &str, default_host: &str, default_port: u16) -> (String, u16) {
    let input = input.trim();
    if input.is_empty() {
        return (default_host.to_string(), default_port);
    }
    // ":port" format
    if let Some(port_str) = input.strip_prefix(':') {
        if let Ok(port) = port_str.parse::<u16>() {
            return (default_host.to_string(), port);
        }
    }
    // "host:port" format
    if let Some((host, port_str)) = input.rsplit_once(':') {
        if let Ok(port) = port_str.parse::<u16>() {
            let h = if host.is_empty() { default_host.to_string() } else { host.to_string() };
            return (h, port);
        }
    }
    // "host" only (no colon)
    if let Ok(port) = input.parse::<u16>() {
        // Just a number — treat as port
        return (default_host.to_string(), port);
    }
    // Just a hostname
    (input.to_string(), default_port)
}

fn make_proxy_name(protocol: &str, existing: &[WizardProxy]) -> String {
    let base = protocol;
    if !existing.iter().any(|p| p.name == base) {
        return base.to_string();
    }
    for i in 2..100 {
        let name = format!("{}-{}", base, i);
        if !existing.iter().any(|p| p.name == name) {
            return name;
        }
    }
    format!("{}-{}", base, existing.len() + 1)
}

// ─── Config generation ──────────────────────────────────────────────────────

fn config_dir() -> Result<PathBuf> {
    // Use XDG convention (~/.config/ocular) consistently across platforms,
    // matching load_config()'s candidate priority order.
    let dir = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("ocular"))
        .ok_or_else(|| anyhow::anyhow!("cannot determine config directory"))?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn generate_config(proxies: &[WizardProxy]) -> String {
    let mut out = String::new();
    out.push_str("# Ocular configuration \u{2014} generated by ocular setup\n");
    out.push_str("# Docs: https://github.com/beyondlex/ocular\n\n");
    out.push_str("theme = \"tokyo-night-storm\"\n");
    out.push_str("event_format = \"%{5}index %time %{-8}component %latency %command\"\n\n");

    out.push_str("[exclude.redis]\npatterns = [\"PING\", \"INFO\"]\n\n");
    out.push_str("[exclude.mysql]\npatterns = [\"SELECT 1\"]\n\n");
    out.push_str("[exclude.postgres]\npatterns = [\"SET \", \"DEALLOCATE\"]\n\n");
    out.push_str("[exclude.rabbitmq]\npatterns = [\"Heartbeat\"]\n\n");

    for p in proxies {
        out.push_str("[[proxy]]\n");
        out.push_str(&format!("name = \"{}\"\n", p.name));
        out.push_str(&format!("protocol = \"{}\"\n", p.protocol));
        match p.mode {
            Mode::Proxy => {
                out.push_str(&format!("listen = \"{}\"\n", p.listen_addr));
                out.push_str(&format!("remote = \"{}:{}\"\n", p.host, p.port));
            }
            Mode::Capture => {
                out.push_str("mode = \"capture\"\n");
                out.push_str(&format!("interface = \"{}\"\n", p.interface));
                out.push_str(&format!("remote = \"{}:{}\"\n", p.host, p.port));
            }
        }
        out.push('\n');
    }
    out
}

// ─── Step rendering helpers ─────────────────────────────────────────────────

fn print_question(text: &str) {
    Term::color(Color::White);
    Term::bold_on();
    Term::print("  ? ");
    Term::bold_off();
    Term::reset();
    Term::println(text);
}

fn print_input_label(label: &str, _default: &str) {
    Term::color(Color::White);
    Term::bold_on();
    Term::print("  ? ");
    Term::bold_off();
    Term::reset();
    Term::color(Color::White);
    Term::print(&format!("{}:", label));
    Term::reset();
    Term::println("");
    Term::flush();
}

/// Print a breadcrumb trail showing confirmed selections.
/// Only shows steps that have been completed; the current step is not shown
/// to avoid displaying stale values (e.g., protocol from a previous round).
fn print_breadcrumb(step: usize, mode: Mode, proto_idx: usize, loc_idx: usize, host: &str, port: u16) {
    let sep = " \u{203a} "; // › thin arrow
    let mut parts: Vec<&str> = Vec::new();

    // Mode — always shown once step > 1 (step 1 is selecting it)
    if step > 1 {
        parts.push(match mode { Mode::Proxy => "Proxy", Mode::Capture => "Capture" });
    }
    // Protocol — shown once step > 2 (step 2 is selecting it)
    if step > 2 {
        parts.push(PROTOCOLS[proto_idx].1);
    }
    // Location — shown once step > 3 (step 3 is selecting it)
    if step > 3 {
        if loc_idx == 0 {
            parts.push("Local");
        } else if !host.is_empty() {
            // Remote: once host is entered, show actual host instead of "Remote"
            parts.push(host);
        } else {
            parts.push("Remote");
        }
    }
    // Remote port sub-step: append "Port" crumb
    if step == 4 && loc_idx == 1 && !host.is_empty() && port == 0 {
        parts.push("Port");
    }

    if parts.is_empty() {
        // Step 1: no breadcrumb yet, print blank lines to keep layout stable
        println!();
        println!();
        return;
    }

    Term::print("  ");
    for (i, label) in parts.iter().enumerate() {
        if i > 0 {
            Term::color(Color::DarkGrey);
            Term::print(sep);
            Term::reset();
        }
        // Last item is the most recent — highlight it
        if i == parts.len() - 1 {
            Term::color(Color::Cyan);
            Term::bold_on();
            Term::print(label);
            Term::bold_off();
            Term::reset();
        } else {
            Term::color(Color::DarkGrey);
            Term::print(label);
            Term::reset();
        }
    }
    Term::println("");
    println!();
}

// ─── Screen rendering ───────────────────────────────────────────────────────

/// Clear the screen and render the persistent header:
///   - Logo + version
///   - Already-added proxies summary (if any)
///   - Blank line ready for the next step
fn render_screen(proxies: &[WizardProxy]) {
    Term::clear_screen();
    println!();
    Term::color(Color::Cyan);
    Term::bold_on();
    println!("  \u{25cb} Ocular v{}", env!("CARGO_PKG_VERSION"));
    Term::bold_off();
    Term::reset();
    println!();

    if proxies.is_empty() {
        Term::color(Color::White);
        println!("  Setting up your first proxy.");
        Term::reset();
    } else {
        Term::color(Color::Green);
        Term::bold_on();
        Term::print("  \u{2713} ");
        Term::bold_off();
        Term::reset();
        Term::color(Color::White);
        Term::println(&format!("{} prox{} configured:", proxies.len(), if proxies.len() == 1 { "y" } else { "ies" }));
        Term::reset();
        for p in proxies {
            // Status indicator
            match p.reachable {
                Some(true) => {
                    Term::color(Color::Green);
                    Term::print("  \u{2713} ");
                }
                Some(false) => {
                    Term::color(Color::Yellow);
                    Term::print("  \u{26a0} ");
                }
                None => {
                    Term::color(Color::DarkGrey);
                    Term::print("  \u{280b} ");
                }
            }
            Term::reset();
            Term::color(Color::Cyan);
            Term::print(&format!("{:<12}", p.name));
            Term::reset();
            match p.mode {
                Mode::Proxy => {
                    Term::print(&format!("{} \u{2192} {}:{}", p.listen_addr, p.host, p.port));
                }
                Mode::Capture => {
                    Term::print(&format!("capture {} \u{2192} {}:{}", p.interface, p.host, p.port));
                }
            }
            Term::println("");
        }
    }
    println!();
    Term::color(Color::DarkGrey);
    println!("  {}", "\u{2500}".repeat(50));
    Term::reset();
    println!();
}

// ─── Main wizard ────────────────────────────────────────────────────────────

pub async fn run_wizard() -> Result<Option<PathBuf>> {
    // Check if config already exists
    if let Ok(dir) = config_dir() {
        let existing = dir.join("ocular.toml");
        if existing.exists() {
            Term::clear_screen();
            println!();
            Term::color(Color::Cyan);
            Term::bold_on();
            println!("  \u{25cb} Ocular v{}", env!("CARGO_PKG_VERSION"));
            Term::bold_off();
            Term::reset();
            println!();
            Term::color(Color::Yellow);
            Term::print("  \u{26a0} ");
            Term::reset();
            Term::println(&format!("Config already exists at {}", existing.display()));
            println!();
            Term::println("  This will overwrite your existing configuration.");
            println!();
            let overwrite = match read_selection(&[
                ("Cancel", "keep existing config"),
                ("Overwrite", "replace with new config"),
            ], "\u{2191}\u{2193} navigate  Enter confirm  Esc back") {
                Ok(Some(i)) => i == 1,
                Ok(None) => return Ok(None),
                Err(_) => return Ok(None),
            };
            if !overwrite {
                println!();
                return Ok(None);
            }
        }
    }

    let mut proxies: Vec<WizardProxy> = Vec::new();

    // Wizard state — reset to Step 1 for each new proxy attempt
    let mut mode = Mode::Proxy;
    let mut proto_idx: usize = 0;
    let mut loc_idx: usize = 0;
    let mut host = String::new();
    let mut port: u16 = 0;
    let mut step: usize = 1;

    loop {
        match step {
            // ─── Step 1: Mode ─────────────────────────────────────────
            1 => {
                render_screen(&proxies);
                print_breadcrumb(step, mode, proto_idx, loc_idx, &host, port);
                print_question("How do you want to observe traffic?");
                match read_selection(&[
                    ("Proxy", "sit between app and service (handles SSL)"),
                    ("Capture", "passive sniffing, zero config on app (needs sudo)"),
                ], SELECTION_HINT) {
                    Ok(Some(i)) => {
                        mode = if i == 0 { Mode::Proxy } else { Mode::Capture };
                        step = 2;
                    }
                    Ok(None) => return Ok(None), // Esc at step 1 → quit wizard
                    Err(_) => return Ok(None),
                }
            }

            // ─── Step 2: Protocol ─────────────────────────────────────
            2 => {
                render_screen(&proxies);
                print_breadcrumb(step, mode, proto_idx, loc_idx, &host, port);
                print_question("Which protocol?");
                let proto_options: Vec<(String, String)> = PROTOCOLS
                    .iter()
                    .map(|(_key, label, port)| (label.to_string(), format!("port {}", port)))
                    .collect();
                let proto_refs: Vec<(&str, &str)> = proto_options
                    .iter()
                    .map(|(l, d)| (l.as_str(), d.as_str()))
                    .collect();
                match read_selection(&proto_refs, SELECTION_HINT) {
                    Ok(Some(i)) => {
                        proto_idx = i;
                        step = 3;
                    }
                    Ok(None) => step = 1,
                    Err(_) => return Ok(None),
                }
            }

            // ─── Step 3: Location ─────────────────────────────────────
            3 => {
                let (_k, _l, _p) = PROTOCOLS[proto_idx];
                render_screen(&proxies);
                print_breadcrumb(step, mode, proto_idx, loc_idx, &host, port);
                print_question(&format!("Where is your {} server?", proto_label_display(_k)));
                match read_selection(&[
                    ("Local", "127.0.0.1"),
                    ("Remote", "another machine"),
                ], SELECTION_HINT) {
                    Ok(Some(i)) => {
                        loc_idx = i;
                        step = 4;
                    }
                    Ok(None) => step = 2,
                    Err(_) => return Ok(None),
                }
            }

            // ─── Step 4: Address ──────────────────────────────────────
            4 => {
                let (_k, _l, default_port) = PROTOCOLS[proto_idx];
                if loc_idx == 0 {
                    // Local
                    render_screen(&proxies);
                    print_breadcrumb(step, mode, proto_idx, loc_idx, &host, port);
                    let default_addr = format!("127.0.0.1:{}", default_port);
                    print_input_label("Target address", &default_addr);
                    match read_text(&default_addr, INPUT_HINT) {
                        Ok(Some(addr_input)) => {
                            let (h, p) = parse_address(&addr_input, "127.0.0.1", default_port);
                            host = h;
                            port = p;
                            step = 5;
                        }
                        Ok(None) => step = 3,
                        Err(_) => return Ok(None),
                    }
                } else if host.is_empty() {
                    // Remote — host (first pass)
                    render_screen(&proxies);
                    print_breadcrumb(step, mode, proto_idx, loc_idx, &host, port);
                    print_input_label("Remote hostname or IP", "127.0.0.1");
                    match read_text("127.0.0.1", INPUT_HINT) {
                        Ok(Some(host_input)) => {
                            host = if host_input.is_empty() { "127.0.0.1".to_string() } else { host_input };
                            // proceed to port step (still step 4, but host is now set)
                        }
                        Ok(None) => {
                            step = 3;
                        }
                        Err(_) => return Ok(None),
                    }
                } else {
                    // Remote — port
                    render_screen(&proxies);
                    print_breadcrumb(step, mode, proto_idx, loc_idx, &host, port);
                    let port_default = format!("{}", default_port);
                    print_input_label("Port", &port_default);
                    match read_text(&port_default, INPUT_HINT) {
                        Ok(Some(port_input)) => {
                            port = port_input.parse().unwrap_or(default_port);
                            step = 5;
                        }
                        Ok(None) => {
                            host.clear(); // go back to host prompt
                        }
                        Err(_) => return Ok(None),
                    }
                }
            }

            // ─── Step 5: Connectivity + confirm + add another ─────────
            5 => {
                let (proto_key, _proto_label, _default_port) = PROTOCOLS[proto_idx];

                // Build proxy entry and push BEFORE connectivity check so header updates
                let name = make_proxy_name(proto_key, &proxies);
                let (listen_addr, interface) = match mode {
                    Mode::Proxy => (auto_assign_listen_port(port), String::new()),
                    Mode::Capture => (String::new(), detect_interface(&host)),
                };

                // Check for duplicate remote in different mode
                let remote_addr = format!("{}:{}", host, port);
                let duplicate_warning = proxies.iter().any(|p| {
                    let p_remote = format!("{}:{}", p.host, p.port);
                    p_remote == remote_addr && p.mode != mode
                });

                proxies.push(WizardProxy {
                    name: name.clone(),
                    protocol: proto_key.to_string(),
                    mode,
                    host: host.clone(),
                    port,
                    listen_addr: listen_addr.clone(),
                    interface: interface.clone(),
                    reachable: None,
                });

                // Re-render screen — header now shows the new proxy in configured list
                render_screen(&proxies);
                print_breadcrumb(step, mode, proto_idx, loc_idx, &host, port);

                // Connectivity check (inline, don't clear)
                Term::color(Color::DarkGrey);
                Term::print("  \u{280b} ");
                Term::reset();
                Term::print(&format!("Testing connection to {}:{}...", host, port));
                Term::flush();

                let reachable = match check_connectivity(&host, port).await {
                    Some(duration) => {
                        Term::clear_line();
                        Term::color(Color::Green);
                        Term::print("  \u{2713} ");
                        Term::reset();
                        Term::println(&format!(
                            "Connected to {}:{} in {:.1}ms",
                            host,
                            port,
                            duration.as_secs_f64() * 1000.0
                        ));
                        true
                    }
                    None => {
                        Term::clear_line();
                        Term::color(Color::Yellow);
                        Term::print("  \u{26a0} ");
                        Term::reset();
                        Term::println(&format!("Cannot reach {}:{}", host, port));
                        Term::color(Color::DarkGrey);
                        Term::println(&format!("    Hint: is the service running? Try: {}", connectivity_hint(proto_key, &host, port)));
                        Term::println("    Continuing anyway \u{2014} you can fix this later.");
                        Term::reset();
                        false
                    }
                };

                // Update reachable status and re-render header
                if let Some(last) = proxies.last_mut() {
                    last.reachable = Some(reachable);
                }
                render_screen(&proxies);
                print_breadcrumb(step, mode, proto_idx, loc_idx, &host, port);

                // Show added confirmation inline
                println!();
                Term::color(Color::Green);
                Term::print("  \u{2713} ");
                Term::reset();
                match mode {
                    Mode::Proxy => {
                        Term::println(&format!("Added: {} ({} \u{2192} {}:{})", name, listen_addr, host, port));
                    }
                    Mode::Capture => {
                        Term::println(&format!("Added: {} (capture on {} \u{2192} {}:{})", name, interface, host, port));
                    }
                }

                // Warn about duplicate remote in different mode
                if duplicate_warning {
                    Term::color(Color::Yellow);
                    Term::print("  \u{26a0} ");
                    Term::reset();
                    Term::color(Color::DarkGrey);
                    Term::println(&format!("{}:{} is also in another mode — events will appear twice", host, port));
                    Term::reset();
                }

                // Add another?
                println!();
                print_question("Add another proxy?");
                match read_selection(&[
                    ("No, I'm done", ""),
                    ("Yes, add another", ""),
                ], SELECTION_HINT) {
                    Ok(Some(0)) => break,
                    Ok(Some(_)) => {
                        // Reset wizard state for next proxy; go back to step 1
                        host.clear();
                        port = 0;
                        step = 1;
                    }
                    Ok(None) => {
                        // Esc at "add another" → go back to address step to edit
                        // Pop the just-added proxy so user can re-confirm
                        proxies.pop();
                        step = 4;
                    }
                    Err(_) => return Ok(None),
                }
            }

            _ => break,
        }
    }

    if proxies.is_empty() {
        Term::clear_screen();
        Term::color(Color::DarkGrey);
        Term::println("  No proxies configured. Exiting setup.");
        Term::reset();
        println!();
        return Ok(None);
    }

    // ─── Final: Write config + summary ────────────────────────────────
    let dir = config_dir()?;
    let config_path = dir.join("ocular.toml");
    let content = generate_config(&proxies);
    std::fs::write(&config_path, &content)?;

    Term::clear_screen();
    println!();
    Term::color(Color::Cyan);
    Term::bold_on();
    println!("  \u{25cb} Ocular v{}", env!("CARGO_PKG_VERSION"));
    Term::bold_off();
    Term::reset();
    println!();
    Term::color(Color::Green);
    Term::print("  \u{2713} ");
    Term::reset();
    Term::println(&format!("Config written to {}", config_path.display()));
    println!();

    Term::color(Color::White);
    Term::bold_on();
    Term::println("  Proxies:");
    Term::bold_off();
    Term::reset();
    for p in &proxies {
        let mode_str = match p.mode {
            Mode::Proxy => "proxy",
            Mode::Capture => "capture",
        };
        Term::color(Color::DarkGrey);
        Term::print("    ");
        Term::reset();
        Term::color(Color::Cyan);
        Term::print(&format!("{:<14}", p.name));
        Term::reset();
        match p.mode {
            Mode::Proxy => {
                Term::print(&format!("{} \u{2192} {}:{}", p.listen_addr, p.host, p.port));
            }
            Mode::Capture => {
                Term::print(&format!("capture {} \u{2192} {}:{}", p.interface, p.host, p.port));
            }
        }
        Term::color(Color::DarkGrey);
        Term::println(&format!("  [{}]", mode_str));
        Term::reset();
    }
    println!();

    Ok(Some(config_path))
}

fn proto_label_display(key: &str) -> &str {
    match key {
        "redis" => "Redis",
        "mysql" => "MySQL",
        "postgres" => "PostgreSQL",
        "mongodb" => "MongoDB",
        "rabbitmq" => "RabbitMQ",
        "kafka" => "Kafka",
        "elasticsearch" => "Elasticsearch",
        "memcached" => "Memcached",
        _ => key,
    }
}
