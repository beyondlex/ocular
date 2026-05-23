/// Generic HTTP/1.1 protocol parser for Ocular.
/// Parses request line, headers, and body. Used for Elasticsearch and other HTTP services.

/// Parse an HTTP request buffer, returning "METHOD /path" summary.
pub fn parse_http_request(buf: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(buf).ok()?;
    let first_line = s.lines().next()?;
    // "GET /index/_search HTTP/1.1"
    let mut parts = first_line.splitn(3, ' ');
    let method = parts.next()?;
    let path = parts.next()?;
    Some(format!("{} {}", method, path))
}

/// Extract full HTTP request (method + path + body if present).
pub fn extract_http_full_command(buf: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(buf).ok()?;
    let first_line = s.lines().next()?;
    let mut parts = first_line.splitn(3, ' ');
    let method = parts.next()?;
    let path = parts.next()?;

    let body = extract_body_from_http(s);
    if body.is_empty() {
        Some(format!("{} {}", method, path))
    } else {
        Some(format!("{} {}\n{}", method, path, body))
    }
}

/// Parse an HTTP response buffer, returning status summary.
pub fn parse_http_response(buf: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(buf).ok()?;
    let first_line = s.lines().next()?;
    // "HTTP/1.1 200 OK"
    let mut parts = first_line.splitn(3, ' ');
    let _version = parts.next()?;
    let status = parts.next()?;
    let reason = parts.next().unwrap_or("");
    Some(format!("{} {}", status, reason))
}

/// Format detailed HTTP response (status + body).
pub fn format_http_response_detail(buf: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(buf).ok()?;
    let first_line = s.lines().next()?;
    let body = extract_body_from_http(s);
    if body.is_empty() {
        Some(first_line.to_string())
    } else {
        // Try to pretty-print JSON body (simple indent)
        let pretty = simple_json_format(&body);
        Some(pretty)
    }
}

/// Check if an HTTP response is complete (has full headers + body per Content-Length).
pub fn http_response_complete(buf: &[u8]) -> bool {
    let Some(s) = std::str::from_utf8(buf).ok() else { return false };
    let Some(header_end) = s.find("\r\n\r\n") else { return false };
    let headers = &s[..header_end];

    // Check for chunked transfer encoding
    if headers.to_lowercase().contains("transfer-encoding: chunked") {
        // Chunked: complete when body ends with "0\r\n\r\n"
        return buf.ends_with(b"0\r\n\r\n") || buf.ends_with(b"0\r\n\r\n");
    }

    // Content-Length based
    if let Some(cl) = extract_content_length(headers) {
        let body_start = header_end + 4;
        return buf.len() >= body_start + cl;
    }

    // No Content-Length and not chunked — assume complete after headers
    true
}

/// Check if an HTTP request is complete.
pub fn http_request_complete(buf: &[u8]) -> bool {
    let Some(s) = std::str::from_utf8(buf).ok() else { return false };
    let Some(header_end) = s.find("\r\n\r\n") else { return false };
    let headers = &s[..header_end];

    if let Some(cl) = extract_content_length(headers) {
        let body_start = header_end + 4;
        return buf.len() >= body_start + cl;
    }
    // No body expected (GET, DELETE without body, etc.)
    true
}

// --- Internal helpers ---

fn extract_body_from_http(s: &str) -> String {
    if let Some(pos) = s.find("\r\n\r\n") {
        let body = &s[pos + 4..];
        if !body.is_empty() {
            return body.to_string();
        }
    }
    String::new()
}

fn extract_content_length(headers: &str) -> Option<usize> {
    for line in headers.lines() {
        if line.to_lowercase().starts_with("content-length:") {
            let val = line.split(':').nth(1)?.trim();
            return val.parse().ok();
        }
    }
    None
}

/// Simple JSON formatting — add newlines after { and , for readability.
fn simple_json_format(s: &str) -> String {
    let trimmed = s.trim();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return trimmed.to_string();
    }
    // Try basic indent
    let mut out = String::new();
    let mut indent = 0usize;
    let mut in_string = false;
    let mut prev = '\0';
    for ch in trimmed.chars() {
        if ch == '"' && prev != '\\' {
            in_string = !in_string;
        }
        if in_string {
            out.push(ch);
            prev = ch;
            continue;
        }
        match ch {
            '{' | '[' => {
                out.push(ch);
                indent += 2;
                out.push('\n');
                out.extend(std::iter::repeat(' ').take(indent));
            }
            '}' | ']' => {
                indent = indent.saturating_sub(2);
                out.push('\n');
                out.extend(std::iter::repeat(' ').take(indent));
                out.push(ch);
            }
            ',' => {
                out.push(ch);
                out.push('\n');
                out.extend(std::iter::repeat(' ').take(indent));
            }
            ':' => {
                out.push(':');
                out.push(' ');
            }
            _ if ch.is_whitespace() => {} // skip original whitespace
            _ => { out.push(ch); }
        }
        prev = ch;
    }
    out
}
