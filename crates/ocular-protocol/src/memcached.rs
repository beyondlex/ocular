/// Memcached text protocol parser

/// Parse a memcached request into a human-readable summary
pub fn parse_memcached_request(buf: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(buf).ok()?;
    let line = s.lines().next()?;
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.is_empty() { return None; }
    let cmd = parts[0].to_uppercase();
    match cmd.as_str() {
        "GET" | "GETS" => Some(parts.join(" ")),
        "SET" | "ADD" | "REPLACE" | "APPEND" | "PREPEND" | "CAS" => {
            // set <key> <flags> <exptime> <bytes> [noreply]\r\n<data>\r\n
            if parts.len() >= 5 {
                let key = parts[1];
                let bytes: usize = parts[4].parse().unwrap_or(0);
                // Try to extract the data block
                if let Some(data_start) = s.find("\r\n").map(|i| i + 2) {
                    let data = &s[data_start..];
                    let value = data.get(..bytes.min(64)).unwrap_or(data).trim_end_matches("\r\n");
                    Some(format!("{} {} \"{}\"", cmd, key, value))
                } else {
                    Some(format!("{} {} ({} bytes)", cmd, key, bytes))
                }
            } else {
                Some(parts.join(" "))
            }
        }
        "DELETE" => Some(format!("DELETE {}", parts.get(1).unwrap_or(&""))),
        "INCR" | "DECR" => {
            let key = parts.get(1).unwrap_or(&"");
            let val = parts.get(2).unwrap_or(&"1");
            Some(format!("{} {} {}", cmd, key, val))
        }
        "TOUCH" => {
            let key = parts.get(1).unwrap_or(&"");
            let exp = parts.get(2).unwrap_or(&"0");
            Some(format!("TOUCH {} {}", key, exp))
        }
        "STATS" | "VERSION" | "FLUSH_ALL" | "QUIT" => Some(cmd),
        _ => Some(parts.join(" ")),
    }
}

/// Parse a memcached response into a short summary
pub fn parse_memcached_response(buf: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(buf).ok()?;
    let line = s.lines().next()?;
    match line {
        "STORED" => Some("STORED".into()),
        "NOT_STORED" => Some("NOT_STORED".into()),
        "EXISTS" => Some("EXISTS".into()),
        "NOT_FOUND" => Some("NOT_FOUND".into()),
        "DELETED" => Some("DELETED".into()),
        "TOUCHED" => Some("TOUCHED".into()),
        "OK" => Some("OK".into()),
        "ERROR" => Some("ERROR".into()),
        "END" => Some("(empty)".into()),
        _ if line.starts_with("VALUE ") => {
            // Count VALUE lines
            let count = s.lines().filter(|l| l.starts_with("VALUE ")).count();
            if count == 1 {
                let parts: Vec<&str> = line.split_whitespace().collect();
                let key = parts.get(1).unwrap_or(&"?");
                let bytes: usize = parts.get(3).and_then(|b| b.parse().ok()).unwrap_or(0);
                Some(format!("VALUE {} ({} bytes)", key, bytes))
            } else {
                Some(format!("{} values", count))
            }
        }
        _ if line.starts_with("VERSION ") => Some(line.to_string()),
        _ if line.starts_with("STAT ") => {
            let count = s.lines().filter(|l| l.starts_with("STAT ")).count();
            Some(format!("STATS ({} entries)", count))
        }
        _ if line.starts_with("SERVER_ERROR") || line.starts_with("CLIENT_ERROR") => {
            Some(line.to_string())
        }
        // INCR/DECR response is just a number
        _ => line.parse::<u64>().ok().map(|n| format!("{}", n)),
    }
}

/// Format response detail for the Detail panel
pub fn format_memcached_response_detail(buf: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(buf).ok()?;
    let first = s.lines().next()?;
    if first.starts_with("VALUE ") {
        // Show all VALUE blocks with their data
        let mut result = String::new();
        let mut lines = s.lines().peekable();
        while let Some(line) = lines.next() {
            if line.starts_with("VALUE ") {
                result.push_str(line);
                result.push('\n');
                if let Some(data) = lines.next() {
                    if data != "END" {
                        result.push_str(data);
                        result.push('\n');
                    }
                }
            }
        }
        Some(result.trim_end().to_string())
    } else if first.starts_with("STAT ") {
        Some(s.lines().take_while(|l| l.starts_with("STAT ")).collect::<Vec<_>>().join("\n"))
    } else {
        Some(first.to_string())
    }
}

/// Check if a memcached request is complete (ends with \r\n, and for storage commands includes data block)
pub fn memcached_request_complete(buf: &[u8]) -> bool {
    let s = match std::str::from_utf8(buf) {
        Ok(s) => s,
        Err(_) => return buf.ends_with(b"\r\n"),
    };
    let Some(first_crlf) = s.find("\r\n") else { return false };
    let line = &s[..first_crlf];
    let parts: Vec<&str> = line.split_whitespace().collect();
    let cmd = parts.first().map(|c| c.to_uppercase()).unwrap_or_default();
    match cmd.as_str() {
        "SET" | "ADD" | "REPLACE" | "APPEND" | "PREPEND" | "CAS" => {
            // Need command line + data block + \r\n
            let bytes: usize = parts.get(4).and_then(|b| b.parse().ok()).unwrap_or(0);
            let expected = first_crlf + 2 + bytes + 2;
            buf.len() >= expected
        }
        _ => buf.ends_with(b"\r\n"),
    }
}

/// Check if a memcached response is complete
pub fn memcached_response_complete(buf: &[u8]) -> bool {
    let s = match std::str::from_utf8(buf) {
        Ok(s) => s,
        Err(_) => return buf.ends_with(b"\r\n"),
    };
    // VALUE responses end with "END\r\n"
    if s.starts_with("VALUE ") {
        return s.ends_with("END\r\n");
    }
    // STAT responses end with "END\r\n"
    if s.starts_with("STAT ") {
        return s.ends_with("END\r\n");
    }
    // Single-line responses
    s.ends_with("\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_get() {
        assert_eq!(parse_memcached_request(b"get user:1\r\n"), Some("get user:1".into()));
    }

    #[test]
    fn test_parse_set() {
        let req = b"set user:1 0 300 5\r\nhello\r\n";
        assert_eq!(parse_memcached_request(req), Some("SET user:1 \"hello\"".into()));
    }

    #[test]
    fn test_parse_response_stored() {
        assert_eq!(parse_memcached_response(b"STORED\r\n"), Some("STORED".into()));
    }

    #[test]
    fn test_parse_response_value() {
        let resp = b"VALUE user:1 0 5\r\nhello\r\nEND\r\n";
        assert_eq!(parse_memcached_response(resp), Some("VALUE user:1 (5 bytes)".into()));
    }

    #[test]
    fn test_request_complete_get() {
        assert!(memcached_request_complete(b"get key\r\n"));
        assert!(!memcached_request_complete(b"get key"));
    }

    #[test]
    fn test_request_complete_set() {
        assert!(memcached_request_complete(b"set k 0 0 3\r\nabc\r\n"));
        assert!(!memcached_request_complete(b"set k 0 0 3\r\nab"));
    }

    #[test]
    fn test_response_complete_value() {
        assert!(memcached_response_complete(b"VALUE k 0 3\r\nabc\r\nEND\r\n"));
        assert!(!memcached_response_complete(b"VALUE k 0 3\r\nabc\r\n"));
    }
}
