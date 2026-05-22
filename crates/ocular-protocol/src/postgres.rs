/// PostgreSQL wire protocol parser (v3)
///
/// Message format: [type:1][length:4 (includes self)][payload...]
/// Startup message has no type byte: [length:4][protocol_version:4][params...]

/// Parse a client→server message, return human-readable summary
pub fn parse_postgres_request(buf: &[u8]) -> Option<String> {
    if buf.is_empty() { return None; }

    // Startup message or SSL request: no type byte, starts with [length:4][code:4]
    // Detect by checking if first byte could be a valid message type
    let first = buf[0];
    let is_typed_msg = matches!(first, b'Q' | b'P' | b'B' | b'E' | b'D' | b'S' | b'X' | b'C' | b'p' | b'H' | b'F' | b'd' | b'c' | b'f');

    if !is_typed_msg && buf.len() >= 8 {
        let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        let version = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        if version == 196608 {
            // Protocol 3.0 startup
            let end = len.min(buf.len());
            let params = parse_startup_params(&buf[8..end]);
            return Some(format!("Startup user={}", params));
        }
        if version == 80877103 {
            return Some("SSLRequest".into());
        }
        // Cancel request
        if version == 80877102 {
            return Some("CancelRequest".into());
        }
    }

    if !is_typed_msg { return None; }

    let msg_type = first;
    if buf.len() < 5 { return None; }
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    if buf.len() < 1 + len { return None; }
    let payload = &buf[5..1 + len];

    match msg_type {
        b'Q' => {
            // Simple query
            let sql = read_cstr(payload);
            let truncated: String = sql.chars().take(120).collect();
            if truncated.len() < sql.len() {
                Some(format!("{}...", truncated))
            } else {
                Some(truncated)
            }
        }
        b'P' => {
            // Parse (prepared statement)
            let stmt = read_cstr(payload);
            let rest = &payload[stmt.len() + 1..];
            let query = read_cstr(rest);
            let q: String = query.chars().take(100).collect();
            if stmt.is_empty() {
                Some(format!("PREPARE {}", q))
            } else {
                Some(format!("PREPARE [{}] {}", stmt, q))
            }
        }
        b'B' => Some("BIND".into()),
        b'E' => {
            // Execute
            let portal = read_cstr(payload);
            if portal.is_empty() {
                Some("EXECUTE".into())
            } else {
                Some(format!("EXECUTE [{}]", portal))
            }
        }
        b'D' => {
            // Describe
            let kind = if !payload.is_empty() { payload[0] } else { 0 };
            let name = if payload.len() > 1 { read_cstr(&payload[1..]) } else { String::new() };
            match kind {
                b'S' => Some(format!("DESCRIBE STMT {}", name)),
                b'P' => Some(format!("DESCRIBE PORTAL {}", name)),
                _ => Some("DESCRIBE".into()),
            }
        }
        b'S' => Some("SYNC".into()),
        b'X' => Some("TERMINATE".into()),
        b'C' => {
            // Close
            let kind = if !payload.is_empty() { payload[0] } else { 0 };
            let name = if payload.len() > 1 { read_cstr(&payload[1..]) } else { String::new() };
            match kind {
                b'S' => Some(format!("CLOSE STMT {}", name)),
                b'P' => Some(format!("CLOSE PORTAL {}", name)),
                _ => Some("CLOSE".into()),
            }
        }
        b'p' => Some("PasswordMessage".into()),
        b'H' => Some("FLUSH".into()),
        _ => None,
    }
}

/// Extract full SQL from request (no truncation)
pub fn extract_postgres_full_command(buf: &[u8]) -> Option<String> {
    if buf.is_empty() { return None; }
    let first = buf[0];
    let is_typed = matches!(first, b'Q' | b'P' | b'B' | b'E' | b'D' | b'S' | b'X' | b'C' | b'p' | b'H' | b'F' | b'd' | b'c' | b'f');
    if !is_typed { return parse_postgres_request(buf); }
    if buf.len() < 5 { return None; }
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    if buf.len() < 1 + len { return None; }
    let payload = &buf[5..1 + len];
    match first {
        b'Q' => Some(read_cstr(payload)),
        b'P' => {
            let stmt = read_cstr(payload);
            let rest = &payload[stmt.len() + 1..];
            Some(read_cstr(rest))
        }
        _ => parse_postgres_request(buf),
    }
}

/// Parse a server→client message, return short summary
/// Scans for the most important message in a multi-message buffer.
pub fn parse_postgres_response(buf: &[u8]) -> Option<String> {
    if buf.is_empty() { return None; }

    // SSL response: single byte 'N' (no SSL) or 'S' (SSL)
    if buf.len() == 1 {
        return match buf[0] {
            b'N' => Some("SSLResponse: No".into()),
            b'S' => Some("SSLResponse: Yes".into()),
            _ => None,
        };
    }

    // Scan all messages, prefer Error/CommandComplete over Auth/ReadyForQuery
    let mut result: Option<String> = None;
    let mut pos = 0;
    while pos + 5 <= buf.len() {
        let msg_type = buf[pos];
        let len = u32::from_be_bytes([buf[pos+1], buf[pos+2], buf[pos+3], buf[pos+4]]) as usize;
        if pos + 1 + len > buf.len() { break; }
        let payload = &buf[pos+5..pos+1+len];

        let parsed = parse_single_response(msg_type, payload);
        if let Some(ref p) = parsed {
            // Error/CommandComplete take priority
            if msg_type == b'E' || msg_type == b'C' {
                return parsed;
            }
            // Keep first meaningful result as fallback
            if result.is_none() {
                result = parsed;
            }
        }
        pos += 1 + len;
    }
    result
}

fn parse_single_response(msg_type: u8, payload: &[u8]) -> Option<String> {

    match msg_type {
        b'R' => {
            // Authentication
            if payload.len() >= 4 {
                let auth_type = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
                match auth_type {
                    0 => Some("AuthenticationOk".into()),
                    3 => Some("AuthenticationCleartextPassword".into()),
                    5 => Some("AuthenticationMD5Password".into()),
                    10 => Some("AuthenticationSASL".into()),
                    11 => Some("AuthenticationSASLContinue".into()),
                    12 => Some("AuthenticationSASLFinal".into()),
                    _ => Some(format!("Authentication({})", auth_type)),
                }
            } else {
                Some("Authentication".into())
            }
        }
        b'T' => {
            // RowDescription
            if payload.len() >= 2 {
                let col_count = u16::from_be_bytes([payload[0], payload[1]]);
                Some(format!("RowDescription ({} cols)", col_count))
            } else {
                Some("RowDescription".into())
            }
        }
        b'D' => Some("DataRow".into()),
        b'C' => {
            // CommandComplete
            let tag = read_cstr(payload);
            Some(format!("OK: {}", tag))
        }
        b'Z' => {
            // ReadyForQuery
            let status = if !payload.is_empty() {
                match payload[0] {
                    b'I' => "idle",
                    b'T' => "in transaction",
                    b'E' => "failed transaction",
                    _ => "?",
                }
            } else { "?" };
            Some(format!("Ready ({})", status))
        }
        b'E' => {
            // ErrorResponse
            let msg = parse_error_fields(payload);
            Some(format!("ERROR: {}", msg))
        }
        b'N' => {
            // NoticeResponse
            let msg = parse_error_fields(payload);
            Some(format!("NOTICE: {}", msg))
        }
        b'S' => {
            // ParameterStatus
            let name = read_cstr(payload);
            let rest = &payload[name.len() + 1..];
            let value = read_cstr(rest);
            Some(format!("Set {} = {}", name, value))
        }
        b'K' => Some("BackendKeyData".into()),
        b'1' => Some("ParseComplete".into()),
        b'2' => Some("BindComplete".into()),
        b'3' => Some("CloseComplete".into()),
        b'n' => Some("NoData".into()),
        b't' => Some("ParameterDescription".into()),
        b'I' => Some("EmptyQueryResponse".into()),
        _ => None,
    }
}

/// Format response detail for the detail panel
pub fn format_postgres_response_detail(buf: &[u8]) -> Option<String> {
    if buf.is_empty() { return None; }
    // SSL response
    if buf.len() == 1 {
        return parse_postgres_response(buf);
    }
    // Try to parse multiple messages for a complete result
    let mut detail = String::new();
    let mut pos = 0;
    let mut row_count = 0u64;

    while pos < buf.len() {
        if pos + 5 > buf.len() { break; }
        let msg_type = buf[pos];
        let len = u32::from_be_bytes([buf[pos+1], buf[pos+2], buf[pos+3], buf[pos+4]]) as usize;
        if pos + 1 + len > buf.len() { break; }
        let payload = &buf[pos+5..pos+1+len];

        match msg_type {
            b'T' => {
                // RowDescription - extract column names
                if payload.len() >= 2 {
                    let col_count = u16::from_be_bytes([payload[0], payload[1]]) as usize;
                    let mut p = 2;
                    let mut cols = Vec::new();
                    for _ in 0..col_count {
                        let name = read_cstr(&payload[p..]);
                        p += name.len() + 1 + 18; // name + null + 18 bytes of field info
                        cols.push(name);
                    }
                    detail.push_str(&format!("Columns: {}\n", cols.join(" | ")));
                }
            }
            b'D' => {
                row_count += 1;
                if row_count <= 20 {
                    // DataRow: [col_count:2][for each: len:4 (or -1 for NULL), data]
                    if payload.len() >= 2 {
                        let ncols = u16::from_be_bytes([payload[0], payload[1]]) as usize;
                        let mut p = 2;
                        let mut fields = Vec::new();
                        for _ in 0..ncols {
                            if p + 4 > payload.len() { break; }
                            let flen = i32::from_be_bytes([payload[p], payload[p+1], payload[p+2], payload[p+3]]);
                            p += 4;
                            if flen < 0 {
                                fields.push("NULL".to_string());
                            } else {
                                let end = p + flen as usize;
                                if end <= payload.len() {
                                    fields.push(String::from_utf8_lossy(&payload[p..end]).to_string());
                                }
                                p = end;
                            }
                        }
                        detail.push_str(&fields.join(" | "));
                        detail.push('\n');
                    }
                }
            }
            b'C' => {
                let tag = read_cstr(payload);
                detail.push_str(&format!("{} rows\n{}\n", row_count, tag));
            }
            b'E' => {
                let msg = parse_error_fields(payload);
                detail.push_str(&format!("ERROR: {}\n", msg));
            }
            _ => {}
        }
        pos += 1 + len;
    }

    if detail.is_empty() {
        parse_postgres_response(buf)
    } else {
        Some(detail)
    }
}

/// Check if a PostgreSQL response is complete (ends with ReadyForQuery 'Z')
pub fn postgres_response_complete(buf: &[u8]) -> bool {
    if buf.is_empty() { return false; }
    // SSL response: single byte
    if buf.len() == 1 && (buf[0] == b'N' || buf[0] == b'S') {
        return true;
    }
    if buf.len() < 6 { return false; }
    // Check if last message is ReadyForQuery
    let mut pos = 0;
    let mut last_type = 0u8;
    while pos < buf.len() {
        if pos + 5 > buf.len() { break; }
        let msg_type = buf[pos];
        let len = u32::from_be_bytes([buf[pos+1], buf[pos+2], buf[pos+3], buf[pos+4]]) as usize;
        let end = pos + 1 + len;
        if end > buf.len() { break; }
        last_type = msg_type;
        pos = end;
    }
    last_type == b'Z' && pos == buf.len()
}

fn read_cstr(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).to_string()
}

fn parse_startup_params(buf: &[u8]) -> String {
    let mut user = String::new();
    let mut pos = 0;
    while pos < buf.len() {
        let key = read_cstr(&buf[pos..]);
        if key.is_empty() { break; }
        pos += key.len() + 1;
        let val = read_cstr(&buf[pos..]);
        pos += val.len() + 1;
        if key == "user" { user = val; }
    }
    user
}

fn parse_error_fields(buf: &[u8]) -> String {
    let mut msg = String::new();
    let mut pos = 0;
    while pos < buf.len() {
        let field_type = buf[pos];
        if field_type == 0 { break; }
        pos += 1;
        let value = read_cstr(&buf[pos..]);
        pos += value.len() + 1;
        if field_type == b'M' {
            msg = value;
        }
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_query() {
        // 'Q' + length + "SELECT 1\0"
        let sql = b"SELECT 1\0";
        let len = (sql.len() as u32 + 4).to_be_bytes();
        let mut buf = vec![b'Q'];
        buf.extend_from_slice(&len);
        buf.extend_from_slice(sql);
        let result = parse_postgres_request(&buf).unwrap();
        assert_eq!(result, "SELECT 1");
    }

    #[test]
    fn test_parse_command_complete() {
        // 'C' + length + "INSERT 0 1\0"
        let tag = b"INSERT 0 1\0";
        let len = (tag.len() as u32 + 4).to_be_bytes();
        let mut buf = vec![b'C'];
        buf.extend_from_slice(&len);
        buf.extend_from_slice(tag);
        let result = parse_postgres_response(&buf).unwrap();
        assert_eq!(result, "OK: INSERT 0 1");
    }
}
