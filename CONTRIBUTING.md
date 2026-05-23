# Contributing to Ocular

## Adding a New Protocol

Adding a new protocol requires changes in only a few places:

### 1. Create the parser

Create `crates/ocular-protocol/src/yourprotocol.rs`:

```rust
pub fn parse_request(buf: &[u8]) -> Option<String> { ... }
pub fn parse_response(buf: &[u8]) -> Option<String> { ... }
pub fn format_response_detail(buf: &[u8]) -> Option<String> { ... }
pub fn extract_full_command(buf: &[u8]) -> Option<String> { ... }
```

### 2. Implement the ProtocolHandler trait

In `crates/ocular-protocol/src/handlers.rs`:

```rust
pub struct YourProtocolHandler;

impl ProtocolHandler for YourProtocolHandler {
    fn parse_request(&self, buf: &[u8]) -> Option<String> {
        yourprotocol::parse_request(buf)
    }
    fn parse_response(&self, buf: &[u8]) -> Option<String> {
        yourprotocol::parse_response(buf)
    }
    // ... implement other methods as needed
}
```

### 3. Register the protocol

In `crates/ocular-protocol/src/lib.rs`:

1. Add `pub mod yourprotocol;`
2. Add variant to `Protocol` enum
3. Add name mapping in `Protocol::from_str()`
4. Add handler in `get_handler()`

### 4. Add tests

Add `#[cfg(test)] mod tests` in your parser file. Run with `cargo test`.

### 5. (Optional) Buffering

If your protocol needs request/response buffering across TCP reads, implement:
- `needs_request_buffering() -> true`
- `needs_response_buffering() -> true`
- `request_complete(buf) -> bool`
- `response_complete(buf) -> bool`

Then add buffering logic in `crates/ocular-proxy/src/lib.rs` (see HTTP/MySQL as examples).

## Development

```bash
cargo build          # Build
cargo test           # Run tests
cargo clippy         # Lint
RUST_LOG=debug cargo run  # Run with debug logging
```

## Testing

The `testing/` directory contains Docker Compose services and CRUD scripts for each protocol:

```bash
cd testing
docker compose up -d                    # Start backend services
docker compose --profile client up -d   # Start test clients
./run.sh stop                           # Stop everything
```

## Pull Requests

- Keep changes focused — one feature or fix per PR
- Add tests for new protocol parsers
- Run `cargo clippy` before submitting
- Update README if adding user-facing features
