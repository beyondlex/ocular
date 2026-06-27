//! PostgreSQL wire protocol parser (v3)
//!
//! Message format: [type:1][length:4 (includes self)][payload...]
//! Startup message has no type byte: [length:4][protocol_version:4][params...]

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
        b'B' => {
            // Bind
            if let Some(info) = parse_bind_params(payload) {
                let p = info.params.join(", ");
                if info.stmt.is_empty() {
                    Some(format!("BIND params: [{}]", p))
                } else {
                    Some(format!("BIND [{}] params: [{}]", info.stmt, p))
                }
            } else {
                Some("BIND".into())
            }
        }
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
        if let Some(ref _p) = parsed {
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
    format_postgres_response_detail_with_formats(buf, None)
}

/// Same as `format_postgres_response_detail`, but accepts Bind result format codes
/// which override the RowDescription format codes (needed because Describe(Statement)
/// always returns format_code=0, while actual DataRow data may be binary).
///
/// `bind_formats`: per-column true=binary/false=text. If shorter than column count,
/// remaining columns default to false (text). If Some(empty), all columns are text.
pub fn format_postgres_response_detail_with_formats(buf: &[u8], bind_formats: Option<&[bool]>) -> Option<String> {
        if buf.is_empty() { return None; }
    // SSL response
    if buf.len() == 1 {
        return parse_postgres_response(buf);
    }
    // Try to parse multiple messages for a complete result
    let mut detail = String::new();
    let mut pos = 0;
    let mut row_count = 0u64;
    // Track format codes per column (from RowDescription)
    // true = binary, false = text
    let mut col_formats: Vec<bool> = Vec::new();

    while pos < buf.len() {
        if pos + 5 > buf.len() { break; }
        let msg_type = buf[pos];
        let len = u32::from_be_bytes([buf[pos+1], buf[pos+2], buf[pos+3], buf[pos+4]]) as usize;
        if pos + 1 + len > buf.len() { break; }
        let payload = &buf[pos+5..pos+1+len];

        match msg_type {
            b'T' if payload.len() >= 2 => {
                                // RowDescription - extract column names and format codes
                let col_count = u16::from_be_bytes([payload[0], payload[1]]) as usize;
                    let mut p = 2;
                    let mut cols = Vec::new();
                    col_formats.clear();
                    for _ in 0..col_count {
                        let name = read_cstr(&payload[p..]);
                        p += name.len() + 1;
                        // field metadata: table_oid(4) + col_attr(2) + type_oid(4) + type_size(2) + type_mod(4) + format_code(2)
                        if p + 18 <= payload.len() {
                            let format_code = u16::from_be_bytes([payload[p+16], payload[p+17]]);
                            col_formats.push(format_code == 1);
                            p += 18;
                        } else {
                            col_formats.push(false);
                            p = payload.len();
                        }
                        cols.push(name);
                    }
                    // If Bind result format codes are available, use them INSTEAD of
                    // the RowDescription format codes (Describe(Statement) always returns
                    // format_code=0 even when actual data is binary).
                    //
                    // Per PG wire protocol:
                    //   bind_formats.len() == 0 → num_rf=0, all text (no override)
                    //   bind_formats.len() == 1 → single format for ALL columns
                    //   bind_formats.len() >= 2 → per-column format; remaining default text
                    if let Some(bind_fmts) = bind_formats {
                                                if bind_fmts.len() == 1 {
                            let all = bind_fmts[0];
                            for fmt in col_formats.iter_mut() {
                                *fmt = all;
                            }
                        } else {
                            for (i, fmt) in col_formats.iter_mut().enumerate() {
                                if let Some(&bf) = bind_fmts.get(i) {
                                    *fmt = bf;
                                } else {
                                    *fmt = false; // remaining columns default to text
                                }
                            }
                        }
                    }
                    detail.push_str(&format!("Columns: {}\n", cols.join(" | ")));
            }
            b'D' => {
                row_count += 1;
                if row_count <= 20 {
                    // DataRow: [col_count:2][for each: len:4 (or -1 for NULL), data]
                    if payload.len() >= 2 {
                        let ncols = u16::from_be_bytes([payload[0], payload[1]]) as usize;
                        let mut p = 2;
                        let mut fields = Vec::new();
                        for i in 0..ncols {
                            if p + 4 > payload.len() { break; }
                            let flen = i32::from_be_bytes([payload[p], payload[p+1], payload[p+2], payload[p+3]]);
                            p += 4;
                            if flen < 0 {
                                fields.push("NULL".to_string());
                            } else {
                                let end = p + flen as usize;
                                if end <= payload.len() {
                                    // Use Bind result format codes as authoritative (they don't
                                    // depend on RowDescription being in the same response buffer).
                                    // Fall back to RowDescription format codes for simple queries.
                                    let is_binary = if let Some(bind_fmts) = bind_formats {
                                        if bind_fmts.len() == 1 {
                                            bind_fmts[0]  // single format for ALL columns
                                        } else {
                                            bind_fmts.get(i).copied().unwrap_or(false)
                                        }
                                    } else {
                                        col_formats.get(i).copied().unwrap_or(false)
                                    };
                                    if is_binary {
                                        fields.push(decode_binary_field(&payload[p..end]));
                                    } else {
                                        fields.push(String::from_utf8_lossy(&payload[p..end]).to_string());
                                    }
                                }
                                p = end;
                            }
                        }
                        detail.push_str(&fields.join(" | "));
                        detail.push('\n');
                    }
                }
            }
            b'C' if row_count > 0 => {
                detail.push_str(&format!("{} rows\n", row_count));
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

/// Decode a binary-encoded DataRow field value to a human-readable string.
///
/// Strategy:
/// 1. Try UTF-8 text first — many binary-format PG types (TEXT, VARCHAR, CHAR,
///    JSON, NAME) are sent as raw UTF-8 bytes, same as their text format.
/// 2. Only if the data has non-printable bytes (nulls, control chars), fall
///    through to integer/float/timestamp/hex decoding by size.
fn decode_binary_field(buf: &[u8]) -> String {
    // Step 1: try text. Most PG types use raw UTF-8 for their binary encoding.
    if let Ok(s) = std::str::from_utf8(buf) {
        if !s.is_empty() && s.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
            return s.to_string();
        }
    }

    // Step 2: non-text binary — decode by size
    match buf.len() {
        1 => format!("{}", buf[0]),  // boolean or tiny int
        2 => {
            let v = i16::from_be_bytes([buf[0], buf[1]]);
            format!("{}", v)
        }
        4 => {
            let v = i32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
            format!("{}", v)
        }
        8 => {
            let i = i64::from_be_bytes([buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7]]);
            // Could be float8 — try to format as float if it looks plausible
            let f = f64::from_be_bytes([buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7]]);
            if f.is_finite() && f.to_string().len() < 20 {
                format!("{}", f)
            } else {
                format!("{}", i)
            }
        }
        _ => {
            // Try PG NUMERIC (base-10000 encoding, min 10 bytes)
            if buf.len() >= 10 {
                if let Some(s) = try_decode_numeric(buf) {
                    return s;
                }
            }
            // Fallback: hex dump
            let hex: String = buf.iter().map(|b| format!("{:02x}", b)).collect();
            format!("<hex: {}>", hex)
        }
    }
}

/// Attempt to decode a PostgreSQL NUMERIC binary value.
/// Format: ndigits(i16) | weight(i16) | sign(i16) | dscale(i16) | digits(i16×ndigits)
fn try_decode_numeric(buf: &[u8]) -> Option<String> {
    if buf.len() < 8 || (buf.len() - 8) % 2 != 0 {
        return None;
    }
    let ndigits = u16::from_be_bytes([buf[0], buf[1]]) as usize;
    if 8 + ndigits * 2 != buf.len() {
        return None;
    }
    if ndigits == 0 {
        return None;
    }
    let weight = i16::from_be_bytes([buf[2], buf[3]]);
    let sign = u16::from_be_bytes([buf[4], buf[5]]);
    let dscale = u16::from_be_bytes([buf[6], buf[7]]);

    if sign != 0x0000 && sign != 0x4000 && sign != 0xC000 {
        return None;
    }
    if sign == 0xC000 {
        return Some("NaN".into());
    }
    let negative = sign == 0x4000;
    if dscale > 1000 {
        return None;
    }

    let digits: Vec<u16> = (0..ndigits)
        .map(|i| u16::from_be_bytes([buf[8 + i * 2], buf[8 + i * 2 + 1]]))
        .collect();
    if digits.iter().any(|&d| d > 9999) {
        return None;
    }

    // Build base-10000 digit string — pad fractional groups to 4 chars
    let mut groups: Vec<String> = Vec::with_capacity(ndigits);
    for (i, &d) in digits.iter().enumerate() {
        if i == 0 && weight >= 0 {
            groups.push(d.to_string()); // first integer group: no leading zeros
        } else {
            groups.push(format!("{:04}", d));
        }
    }
    let all = groups.join("");

    let int_groups = if weight >= 0 { (weight + 1) as usize } else { 0 };
    let mut int_end = 0usize;
    for i in 0..int_groups.min(ndigits) {
        int_end += groups[i].len();
    }

    let mut int_str = if int_end > 0 {
        all[..int_end].trim_start_matches('0').to_string()
    } else { String::new() };
    if int_str.is_empty() { int_str = "0".to_string(); }
    let mut frac = if int_end < all.len() {
        all[int_end..].to_string()
    } else { String::new() };

    while (frac.len() as u16) < dscale { frac.push('0'); }
    if (frac.len() as u16) > dscale { frac.truncate(dscale as usize); }

    Some(if negative {
        format!("-{}.{}", int_str, frac)
    } else {
        format!("{}.{}", int_str, frac)
    })
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

/// Parsed Bind message info
#[derive(Debug, Clone)]
pub struct BindInfo {
    pub portal: String,
    pub stmt: String,
    pub params: Vec<String>,
    /// Per-column result format code: true = binary, false = text.
    /// Empty means all columns are text (num_rf = 0 in Bind).
    pub result_formats: Vec<bool>,
}

/// Parse Bind (`B`) message payload, extracting parameter values.
/// Bind format: portal(cstring) | stmt(cstring) | num_pf(i16) | pf_codes(i16×N) |
///              num_params(i16) | param_len(i32)+value(bytes) for each | num_rf(i16) | rf_codes(i16×N)
pub fn parse_bind_params(payload: &[u8]) -> Option<BindInfo> {
    let mut pos = 0;

    let portal = read_cstr(payload);
    pos += portal.len() + 1;
    if pos > payload.len() { return None; }

    let rest = &payload[pos..];
    let stmt = read_cstr(rest);
    pos += stmt.len() + 1;
    if pos + 2 > payload.len() { return None; }

    let num_pf = i16::from_be_bytes([payload[pos], payload[pos + 1]]);
    pos += 2;
    let fmt_codes: Vec<i16> = if num_pf > 0 {
        let count = num_pf as usize;
        if pos + count * 2 > payload.len() { return None; }
        let codes: Vec<i16> = (0..count).map(|i| {
            i16::from_be_bytes([payload[pos + i * 2], payload[pos + i * 2 + 1]])
        }).collect();
        pos += count * 2;
        codes
    } else {
        vec![]
    };

    if pos + 2 > payload.len() { return None; }
    let num_params = i16::from_be_bytes([payload[pos], payload[pos + 1]]);
    pos += 2;

    let mut params = Vec::with_capacity(num_params as usize);
    for i in 0..num_params as usize {
        if pos + 4 > payload.len() { return None; }
        let param_len = i32::from_be_bytes([payload[pos], payload[pos + 1], payload[pos + 2], payload[pos + 3]]);
        pos += 4;

        if param_len == -1 {
            params.push("NULL".to_string());
        } else if param_len < 0 {
            params.push("<invalid>".to_string());
        } else {
            let end = pos + param_len as usize;
            if end > payload.len() { return None; }

            let is_binary = if fmt_codes.is_empty() {
                false
            } else if fmt_codes.len() == 1 {
                fmt_codes[0] == 1
            } else if i < fmt_codes.len() {
                fmt_codes[i] == 1
            } else {
                false
            };

            if is_binary {
                params.push(decode_binary_param(&payload[pos..end]));
            } else {
                let val = String::from_utf8_lossy(&payload[pos..end]).to_string();
                params.push(val);
            }
            pos = end;
        }
    }

    // Parse result-column format codes (num_rf + rf_codes)
    let result_formats: Vec<bool> = if pos + 2 <= payload.len() {
        let num_rf = i16::from_be_bytes([payload[pos], payload[pos + 1]]);
        pos += 2;
        if num_rf > 0 {
            let count = num_rf as usize;
            if pos + count * 2 <= payload.len() {
                let codes: Vec<bool> = (0..count).map(|i| {
                    payload[pos + i * 2 + 1] == 1
                }).collect();
                codes
            } else {
                vec![]
            }
        } else {
            vec![] // num_rf = 0 means all columns are text
        }
    } else {
        vec![]
    };

    Some(BindInfo { portal, stmt, params, result_formats })
}

/// Decode a binary parameter value to a human-readable string.
/// Tries integer decoding for standard PG type sizes, falls back to UTF-8 or hex.
fn decode_binary_param(buf: &[u8]) -> String {
    match buf.len() {
        1 => format!("{}", buf[0]),
        2 => {
            let v = i16::from_be_bytes([buf[0], buf[1]]);
            format!("{}", v)
        }
        4 => {
            let v = i32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
            format!("{}", v)
        }
        8 => {
            let v = i64::from_be_bytes([buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7]]);
            format!("{}", v)
        }
        _ => {
            if let Ok(s) = std::str::from_utf8(buf) {
                if s.chars().all(|c| !c.is_control() || c == '\t' || c == '\n') {
                    return format!("'{}'", s);
                }
            }
            format!("<hex: {}>", buf.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().concat())
        }
    }
}

pub fn read_cstr(buf: &[u8]) -> String {
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

    // ─── Edge case tests ─────────────────────────────────────────────────

    #[test]
    fn test_incomplete_message() {
        // Less than 5 bytes
        assert!(parse_postgres_request(&[b'Q', 0x00]).is_none());
        assert!(parse_postgres_request(&[b'Q', 0x00, 0x00, 0x00]).is_none());
    }

    #[test]
    fn test_incomplete_payload() {
        // Header says 20 bytes but only 5 provided
        let buf = vec![b'Q', 0x00, 0x00, 0x00, 0x14, b'S', b'E'];
        assert!(parse_postgres_request(&buf).is_none());
    }

    #[test]
    fn test_empty_buffer() {
        assert!(parse_postgres_request(&[]).is_none());
        assert!(parse_postgres_response(&[]).is_none());
    }

    #[test]
    fn test_ssl_request() {
        // SSLRequest: length(4) + 80877103
        let mut buf = vec![];
        buf.extend_from_slice(&8u32.to_be_bytes());
        buf.extend_from_slice(&80877103u32.to_be_bytes());
        let result = parse_postgres_request(&buf).unwrap();
        assert_eq!(result, "SSLRequest");
    }

    #[test]
    fn test_cancel_request() {
        let mut buf = vec![];
        buf.extend_from_slice(&16u32.to_be_bytes());
        buf.extend_from_slice(&80877102u32.to_be_bytes());
        buf.extend_from_slice(&[0u8; 8]); // process_id + secret_key
        let result = parse_postgres_request(&buf).unwrap();
        assert_eq!(result, "CancelRequest");
    }

    #[test]
    fn test_startup_message() {
        // Startup: length(4) + version(4)=196608 + params
        let mut buf = vec![];
        let params = b"user\0postgres\0database\0test\0\0";
        let len = 4 + 4 + params.len();
        buf.extend_from_slice(&(len as u32).to_be_bytes());
        buf.extend_from_slice(&196608u32.to_be_bytes());
        buf.extend_from_slice(params);
        let result = parse_postgres_request(&buf).unwrap();
        assert!(result.contains("Startup"));
        assert!(result.contains("postgres"));
    }

    #[test]
    fn test_parse_message() {
        // 'P' (Parse) message
        let stmt = b"\0"; // unnamed
        let query = b"SELECT $1\0";
        let payload_len = stmt.len() + query.len() + 4; // +4 for length field
        let mut buf = vec![b'P'];
        buf.extend_from_slice(&(payload_len as u32).to_be_bytes());
        buf.extend_from_slice(stmt);
        buf.extend_from_slice(query);
        let result = parse_postgres_request(&buf).unwrap();
        assert!(result.contains("PREPARE"));
    }

    #[test]
    fn test_execute_message() {
        let portal = b"\0"; // unnamed
        let payload_len = portal.len() + 4;
        let mut buf = vec![b'E'];
        buf.extend_from_slice(&(payload_len as u32).to_be_bytes());
        buf.extend_from_slice(portal);
        let result = parse_postgres_request(&buf).unwrap();
        assert_eq!(result, "EXECUTE");
    }

    #[test]
    fn test_sync_message() {
        let mut buf = vec![b'S'];
        buf.extend_from_slice(&4u32.to_be_bytes());
        assert_eq!(parse_postgres_request(&buf).unwrap(), "SYNC");
    }

    #[test]
    fn test_terminate_message() {
        let mut buf = vec![b'X'];
        buf.extend_from_slice(&4u32.to_be_bytes());
        assert_eq!(parse_postgres_request(&buf).unwrap(), "TERMINATE");
    }

    #[test]
    fn test_ssl_response() {
        assert_eq!(parse_postgres_response(b"N").unwrap(), "SSLResponse: No");
        assert_eq!(parse_postgres_response(b"S").unwrap(), "SSLResponse: Yes");
    }

    #[test]
    fn test_authentication_ok() {
        let mut buf = vec![b'R'];
        buf.extend_from_slice(&8u32.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes()); // auth type = OK
        assert_eq!(parse_postgres_response(&buf).unwrap(), "AuthenticationOk");
    }

    #[test]
    fn test_ready_for_query() {
        let mut buf = vec![b'Z'];
        buf.extend_from_slice(&5u32.to_be_bytes());
        buf.push(b'I'); // idle
        assert_eq!(parse_postgres_response(&buf).unwrap(), "Ready (idle)");
    }

    #[test]
    fn test_error_response() {
        let mut buf = vec![b'E'];
        let fields = b"SERROR\0C42601\0Msyntax error\0\0";
        buf.extend_from_slice(&((fields.len() + 4) as u32).to_be_bytes());
        buf.extend_from_slice(fields);
        let result = parse_postgres_response(&buf).unwrap();
        assert!(result.contains("ERROR"));
    }

    #[test]
    fn test_multiple_messages_in_buffer() {
        // ParameterStatus + ReadyForQuery
        let mut buf = vec![];
        // ParameterStatus
        let ps_payload = b"server_version\015.0\0";
        buf.push(b'S');
        buf.extend_from_slice(&((ps_payload.len() + 4) as u32).to_be_bytes());
        buf.extend_from_slice(ps_payload);
        // ReadyForQuery
        buf.push(b'Z');
        buf.extend_from_slice(&5u32.to_be_bytes());
        buf.push(b'I');
        let result = parse_postgres_response(&buf).unwrap();
        // Should return the first meaningful result
        assert!(!result.is_empty());
    }

    #[test]
    fn test_response_complete_ready_for_query() {
        let mut buf = vec![b'Z'];
        buf.extend_from_slice(&5u32.to_be_bytes());
        buf.push(b'I');
        assert!(postgres_response_complete(&buf));
    }

    #[test]
    fn test_response_incomplete() {
        assert!(!postgres_response_complete(&[]));
        assert!(!postgres_response_complete(&[b'Z']));
        // Missing final ReadyForQuery
        let mut buf = vec![b'R'];
        buf.extend_from_slice(&8u32.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        assert!(!postgres_response_complete(&buf));
    }

    #[test]
    fn test_response_complete_ssl() {
        assert!(postgres_response_complete(b"N"));
        assert!(postgres_response_complete(b"S"));
    }

    #[test]
    fn test_long_query_truncated() {
        let long_sql = "SELECT ".to_string() + &"x".repeat(200) + "\0";
        let sql_bytes = long_sql.as_bytes();
        let len = (sql_bytes.len() as u32 + 4).to_be_bytes();
        let mut buf = vec![b'Q'];
        buf.extend_from_slice(&len);
        buf.extend_from_slice(sql_bytes);
        let result = parse_postgres_request(&buf).unwrap();
        assert!(result.len() <= 125);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_extract_full_command_no_truncation() {
        let sql = "SELECT * FROM very_long_table_name WHERE id = 12345\0";
        let sql_bytes = sql.as_bytes();
        let len = (sql_bytes.len() as u32 + 4).to_be_bytes();
        let mut buf = vec![b'Q'];
        buf.extend_from_slice(&len);
        buf.extend_from_slice(sql_bytes);
        let result = extract_postgres_full_command(&buf).unwrap();
        assert_eq!(result, "SELECT * FROM very_long_table_name WHERE id = 12345");
    }

    #[test]
    fn test_parse_bind_params_text() {
        let mut payload = vec![];
        payload.extend_from_slice(b"\0"); // portal (unnamed)
        payload.extend_from_slice(b"s1\0"); // stmt
        payload.extend_from_slice(&0i16.to_be_bytes()); // num_pf = 0
        payload.extend_from_slice(&2i16.to_be_bytes()); // num_params = 2
        payload.extend_from_slice(&5i32.to_be_bytes()); // len=5
        payload.extend_from_slice(b"hello");
        payload.extend_from_slice(&2i32.to_be_bytes()); // len=2
        payload.extend_from_slice(b"42");
        payload.extend_from_slice(&0i16.to_be_bytes()); // num_rf = 0

        let info = parse_bind_params(&payload).unwrap();
        assert_eq!(info.portal, "");
        assert_eq!(info.stmt, "s1");
        assert_eq!(info.params, vec!["hello", "42"]);
        assert_eq!(info.result_formats, Vec::<bool>::new()); // num_rf=0 → empty
    }

    #[test]
    fn test_parse_bind_params_binary_result_formats() {
        // Bind with binary result-column format codes (all columns binary)
        let mut payload = vec![];
        payload.extend_from_slice(b"\0"); // portal
        payload.extend_from_slice(b"\0"); // stmt (unnamed)
        payload.extend_from_slice(&0i16.to_be_bytes()); // num_pf = 0
        payload.extend_from_slice(&1i16.to_be_bytes()); // num_params = 1
        payload.extend_from_slice(&3i32.to_be_bytes()); // len=3
        payload.extend_from_slice(b"foo");
        payload.extend_from_slice(&2i16.to_be_bytes()); // num_rf = 2 (2 result columns)
        payload.extend_from_slice(&1i16.to_be_bytes()); // rf_code[0] = 1 (binary)
        payload.extend_from_slice(&0i16.to_be_bytes()); // rf_code[1] = 0 (text)

        let info = parse_bind_params(&payload).unwrap();
        assert_eq!(info.params, vec!["foo"]);
        assert_eq!(info.result_formats, vec![true, false]);
    }

    #[test]
    fn test_parse_bind_params_binary() {
        let mut payload = vec![];
        payload.extend_from_slice(b"\0"); // portal
        payload.extend_from_slice(b"\0"); // stmt (unnamed)
        payload.extend_from_slice(&1i16.to_be_bytes()); // num_pf = 1 (all binary)
        payload.extend_from_slice(&1i16.to_be_bytes()); // fmt_code = 1 (binary)
        payload.extend_from_slice(&2i16.to_be_bytes()); // num_params = 2
        // int4 = 42
        payload.extend_from_slice(&4i32.to_be_bytes());
        payload.extend_from_slice(&42i32.to_be_bytes());
        // int2 = 7
        payload.extend_from_slice(&2i32.to_be_bytes());
        payload.extend_from_slice(&7i16.to_be_bytes());
        payload.extend_from_slice(&0i16.to_be_bytes()); // num_rf = 0

        let info = parse_bind_params(&payload).unwrap();
        assert_eq!(info.params, vec!["42", "7"]);
    }

    #[test]
    fn test_parse_bind_params_null() {
        let mut payload = vec![];
        payload.extend_from_slice(b"\0");
        payload.extend_from_slice(b"\0");
        payload.extend_from_slice(&0i16.to_be_bytes());
        payload.extend_from_slice(&1i16.to_be_bytes()); // num_params = 1
        payload.extend_from_slice(&(-1i32).to_be_bytes()); // NULL
        payload.extend_from_slice(&0i16.to_be_bytes());

        let info = parse_bind_params(&payload).unwrap();
        assert_eq!(info.params[0], "NULL");
    }

    #[test]
    fn test_parse_bind_params_truncated() {
        assert!(parse_bind_params(&[0]).is_none());
    }

    #[test]
    fn test_bind_in_parse_request() {
        let mut payload = vec![];
        payload.extend_from_slice(b"\0");
        payload.extend_from_slice(b"\0");
        payload.extend_from_slice(&0i16.to_be_bytes());
        payload.extend_from_slice(&1i16.to_be_bytes());
        payload.extend_from_slice(&3i32.to_be_bytes());
        payload.extend_from_slice(b"foo");
        payload.extend_from_slice(&0i16.to_be_bytes());

        let len = payload.len() as u32 + 4;
        let mut buf = vec![b'B'];
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&payload);

        let result = parse_postgres_request(&buf).unwrap();
        assert_eq!(result, "BIND params: [foo]");
    }

    #[test]
    fn test_format_detail_with_bind_binary_formats() {
        // Simulate a response to Parse + Describe(Statement) + Bind(binary) + Execute
        // where RowDescription has format_code=0 (from Describe) but Bind says binary.
        // The bind_formats override should make format_postgres_response_detail_with_formats
        // decode column 0 as binary (int4=42) and column 1 as text ("hello").

        fn make_msg(typ: u8, payload: &[u8]) -> Vec<u8> {
            let mut msg = vec![typ];
            msg.extend_from_slice(&((payload.len() as u32 + 4).to_be_bytes()));
            msg.extend_from_slice(payload);
            msg
        }

        // 1. ParseComplete
        let mut buf = make_msg(b'1', &[]);

        // 2. BindComplete
        buf.extend_from_slice(&make_msg(b'2', &[]));

        // 3. RowDescription: 2 columns "id" and "name", both format_code=0 (from Describe)
        let mut rd_payload = vec![];
        rd_payload.extend_from_slice(&2i16.to_be_bytes()); // 2 columns
        // col 0: "id"
        rd_payload.extend_from_slice(b"id\0");
        rd_payload.extend_from_slice(&[0u8; 4]); // table_oid
        rd_payload.extend_from_slice(&[0u8; 2]); // col_attr
        rd_payload.extend_from_slice(&[0u8; 4]); // type_oid (int4 = 23)
        rd_payload.extend_from_slice(&[0u8; 2]); // type_size
        rd_payload.extend_from_slice(&[0u8; 4]); // type_mod
        rd_payload.extend_from_slice(&0i16.to_be_bytes()); // format_code = 0 (text!)
        // col 1: "name"
        rd_payload.extend_from_slice(b"name\0");
        rd_payload.extend_from_slice(&[0u8; 4]);
        rd_payload.extend_from_slice(&[0u8; 2]);
        rd_payload.extend_from_slice(&[0u8; 4]);
        rd_payload.extend_from_slice(&[0u8; 2]);
        rd_payload.extend_from_slice(&[0u8; 4]);
        rd_payload.extend_from_slice(&0i16.to_be_bytes()); // format_code = 0 (text!)
        buf.extend_from_slice(&make_msg(b'T', &rd_payload));

        // 4. DataRow: col0=binary int4=42, col1=text "hello"
        let mut dr_payload = vec![];
        dr_payload.extend_from_slice(&2i16.to_be_bytes()); // 2 columns
        // col0: 4-byte binary int32
        dr_payload.extend_from_slice(&4i32.to_be_bytes()); // length
        dr_payload.extend_from_slice(&42i32.to_be_bytes()); // value
        // col1: 5-byte text
        dr_payload.extend_from_slice(&5i32.to_be_bytes()); // length
        dr_payload.extend_from_slice(b"hello");
        buf.extend_from_slice(&make_msg(b'D', &dr_payload));

        // 5. CommandComplete
        let cc_tag = b"SELECT 1\0";
        buf.extend_from_slice(&make_msg(b'C', cc_tag));

        // 6. ReadyForQuery
        buf.extend_from_slice(&make_msg(b'Z', &[b'I']));

        // WITHOUT bind_formats → column 0 decoded as text (garbled binary)
        let without = format_postgres_response_detail(&buf).unwrap();
        assert!(without.contains("Columns:"));

        // WITH bind_formats (col0=binary, col1=text) → column 0 decoded as "42"
        let bind_fmts = vec![true, false];
        let with = format_postgres_response_detail_with_formats(&buf, Some(&bind_fmts)).unwrap();
        assert!(with.contains("Columns: id | name"));
        assert!(with.contains("42 | hello"), "expected '42 | hello' but got: {with:?}");
        assert!(with.contains("1 rows"));
    }

    #[test]
    fn test_format_detail_with_single_bind_format_all_binary() {
        // num_rf = 1 with format=1 means ALL columns are binary.
        // RowDescription says text (0) but Bind overrides to binary.
        fn make_msg(typ: u8, payload: &[u8]) -> Vec<u8> {
            let mut msg = vec![typ];
            msg.extend_from_slice(&((payload.len() as u32 + 4).to_be_bytes()));
            msg.extend_from_slice(payload);
            msg
        }

        let mut buf = make_msg(b'1', &[]); // ParseComplete
        buf.extend_from_slice(&make_msg(b'2', &[])); // BindComplete

        // RowDescription: 2 cols, both format_code=0
        let mut rd = vec![];
        rd.extend_from_slice(&2i16.to_be_bytes());
        rd.extend_from_slice(b"a\0");
        rd.extend_from_slice(&[0u8; 16]); // metadata
        rd.extend_from_slice(&0i16.to_be_bytes()); // format=0
        rd.extend_from_slice(b"b\0");
        rd.extend_from_slice(&[0u8; 16]);
        rd.extend_from_slice(&0i16.to_be_bytes()); // format=0
        buf.extend_from_slice(&make_msg(b'T', &rd));

        // DataRow: both columns binary int4
        let mut dr = vec![];
        dr.extend_from_slice(&2i16.to_be_bytes());
        dr.extend_from_slice(&4i32.to_be_bytes());
        dr.extend_from_slice(&100i32.to_be_bytes()); // a=100
        dr.extend_from_slice(&4i32.to_be_bytes());
        dr.extend_from_slice(&200i32.to_be_bytes()); // b=200
        buf.extend_from_slice(&make_msg(b'D', &dr));

        buf.extend_from_slice(&make_msg(b'C', b"SELECT 1\0"));
        buf.extend_from_slice(&make_msg(b'Z', &[b'I']));

        // num_rf=1, format=1 (all columns binary)
        let bind_fmts = vec![true];
        let result = format_postgres_response_detail_with_formats(&buf, Some(&bind_fmts)).unwrap();
        assert!(result.contains("100 | 200"), "expected '100 | 200' but got: {result:?}");
    }

    #[test]
    fn test_format_detail_with_single_bind_format_all_text() {
        // num_rf = 1 with format=0 means ALL columns are text.
        // RowDescription says text (0), Bind says text → no change, everything works.
        fn make_msg(typ: u8, payload: &[u8]) -> Vec<u8> {
            let mut msg = vec![typ];
            msg.extend_from_slice(&((payload.len() as u32 + 4).to_be_bytes()));
            msg.extend_from_slice(payload);
            msg
        }

        let mut buf = make_msg(b'1', &[]);
        buf.extend_from_slice(&make_msg(b'2', &[]));

        let mut rd = vec![];
        rd.extend_from_slice(&2i16.to_be_bytes());
        rd.extend_from_slice(b"a\0");
        rd.extend_from_slice(&[0u8; 16]);
        rd.extend_from_slice(&0i16.to_be_bytes());
        rd.extend_from_slice(b"b\0");
        rd.extend_from_slice(&[0u8; 16]);
        rd.extend_from_slice(&0i16.to_be_bytes());
        buf.extend_from_slice(&make_msg(b'T', &rd));

        let mut dr = vec![];
        dr.extend_from_slice(&2i16.to_be_bytes());
        dr.extend_from_slice(&3i32.to_be_bytes());
        dr.extend_from_slice(b"abc");
        dr.extend_from_slice(&3i32.to_be_bytes());
        dr.extend_from_slice(b"def");
        buf.extend_from_slice(&make_msg(b'D', &dr));

        buf.extend_from_slice(&make_msg(b'C', b"SELECT 2\0"));
        buf.extend_from_slice(&make_msg(b'Z', &[b'I']));

        let bind_fmts = vec![false]; // num_rf=1, all text
        let result = format_postgres_response_detail_with_formats(&buf, Some(&bind_fmts)).unwrap();
        assert!(result.contains("abc | def"));
    }

    #[test]
    fn test_format_detail_bind_formats_no_row_description() {
        // Simulate the sqlx flow where RowDescription comes in a previous response
        // (Parse → Describe → Sync) and only DataRows appear in this buffer
        // (Bind → Execute → Sync). col_formats stays empty, but bind_formats
        // should override directly in the DataRow handler.
        fn make_msg(typ: u8, payload: &[u8]) -> Vec<u8> {
            let mut msg = vec![typ];
            msg.extend_from_slice(&((payload.len() as u32 + 4).to_be_bytes()));
            msg.extend_from_slice(payload);
            msg
        }

        let mut buf = make_msg(b'2', &[]); // BindComplete (no RowDescription!)

        // DataRow: col0=binary int4=42, col1=binary int4=99
        let mut dr = vec![];
        dr.extend_from_slice(&2i16.to_be_bytes());
        dr.extend_from_slice(&4i32.to_be_bytes());
        dr.extend_from_slice(&42i32.to_be_bytes());
        dr.extend_from_slice(&4i32.to_be_bytes());
        dr.extend_from_slice(&99i32.to_be_bytes());
        buf.extend_from_slice(&make_msg(b'D', &dr));

        buf.extend_from_slice(&make_msg(b'C', b"SELECT 1\0"));
        buf.extend_from_slice(&make_msg(b'Z', &[b'I']));

        // Bind says num_rf=1, format=1 (all binary)
        let bind_fmts = vec![true];
        let result = format_postgres_response_detail_with_formats(&buf, Some(&bind_fmts)).unwrap();
        assert!(result.contains("42 | 99"), "expected '42 | 99' but got: {result:?}");

        // Without bind_formats (None), the data would be garbled
        let without = format_postgres_response_detail(&buf).unwrap();
        // col_formats is empty → default to text → binary data "" displayed as garbled
        // Just verify it doesn't crash and produces different output
        assert_ne!(without, result);
    }

    #[test]
    fn test_decode_numeric_pg() {
        // numeric(6,5) value 0.02: ndigits=1, weight=-1, sign=pos, dscale=5, digit=200
        let mut buf = vec![];
        buf.extend_from_slice(&1i16.to_be_bytes());  // ndigits
        buf.extend_from_slice(&(-1i16).to_be_bytes()); // weight = -1
        buf.extend_from_slice(&0i16.to_be_bytes());  // sign = positive
        buf.extend_from_slice(&5i16.to_be_bytes());  // dscale = 5
        buf.extend_from_slice(&200i16.to_be_bytes()); // digit = 200

        assert_eq!(try_decode_numeric(&buf).unwrap(), "0.02000");

        // 123.45: ndigits=3, weight=1, sign=pos, dscale=2
        // digit[0]=0, digit[1]=123, digit[2]=4500
        let mut buf2 = vec![];
        buf2.extend_from_slice(&3i16.to_be_bytes());
        buf2.extend_from_slice(&1i16.to_be_bytes());  // weight=1
        buf2.extend_from_slice(&0i16.to_be_bytes());
        buf2.extend_from_slice(&2i16.to_be_bytes());  // dscale=2
        buf2.extend_from_slice(&0i16.to_be_bytes());  // digit[0]=0
        buf2.extend_from_slice(&123i16.to_be_bytes()); // digit[1]=123
        buf2.extend_from_slice(&4500i16.to_be_bytes()); // digit[2]=4500
        assert_eq!(try_decode_numeric(&buf2).unwrap(), "123.45");

        // -42.5: ndigits=2, weight=0, sign=neg, dscale=1
        // digit[0]=42, digit[1]=5000
        let mut buf3 = vec![];
        buf3.extend_from_slice(&2i16.to_be_bytes());
        buf3.extend_from_slice(&0i16.to_be_bytes());  // weight=0
        buf3.extend_from_slice(&0x4000i16.to_be_bytes()); // sign=negative
        buf3.extend_from_slice(&1i16.to_be_bytes());  // dscale=1
        buf3.extend_from_slice(&42i16.to_be_bytes());  // digit[0]=42
        buf3.extend_from_slice(&5000i16.to_be_bytes()); // digit[1]=5000
        assert_eq!(try_decode_numeric(&buf3).unwrap(), "-42.5");

        // NaN
        let mut buf4 = vec![];
        buf4.extend_from_slice(&1i16.to_be_bytes());
        buf4.extend_from_slice(&0i16.to_be_bytes());
        buf4.extend_from_slice(&(-16384i16).to_be_bytes()); // sign = NaN (0xC000 as i16)
        buf4.extend_from_slice(&0i16.to_be_bytes());  // dscale=0
        buf4.extend_from_slice(&0i16.to_be_bytes());  // digit=0
        assert_eq!(try_decode_numeric(&buf4).unwrap(), "NaN");

        // Invalid sign → None
        let mut buf5 = vec![];
        buf5.extend_from_slice(&1i16.to_be_bytes());
        buf5.extend_from_slice(&0i16.to_be_bytes());
        buf5.extend_from_slice(&(-1i16).to_be_bytes()); // invalid sign (0xFFFF)
        buf5.extend_from_slice(&0i16.to_be_bytes());
        buf5.extend_from_slice(&0i16.to_be_bytes());
        assert!(try_decode_numeric(&buf5).is_none());
    }
}
