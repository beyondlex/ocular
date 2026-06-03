use anyhow::{Result, bail};

/// Redis RESP value
#[derive(Debug, Clone, PartialEq)]
pub enum RespValue {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(Option<Vec<u8>>),
    Array(Option<Vec<RespValue>>),
}

impl RespValue {
    /// Format a RESP command as a readable string, e.g. "SET key value"
    pub fn to_command_string(&self) -> String {
        match self {
            RespValue::Array(Some(parts)) => {
                parts.iter().map(|p| match p {
                    RespValue::Bulk(Some(b)) => String::from_utf8_lossy(b).to_string(),
                    other => format!("{:?}", other),
                }).collect::<Vec<_>>().join(" ")
            }
            RespValue::Simple(s) => s.clone(),
            RespValue::Error(e) => format!("ERR: {}", e),
            RespValue::Integer(i) => i.to_string(),
            RespValue::Bulk(Some(b)) => String::from_utf8_lossy(b).to_string(),
            _ => String::from("(nil)"),
        }
    }
}

/// Parse a complete RESP value from a byte buffer. Returns (value, bytes consumed).
pub fn parse_resp(buf: &[u8]) -> Result<Option<(RespValue, usize)>> {
    if buf.is_empty() {
        return Ok(None);
    }
    parse_value(buf, 0)
}

fn parse_value(buf: &[u8], pos: usize) -> Result<Option<(RespValue, usize)>> {
    if pos >= buf.len() {
        return Ok(None);
    }
    match buf[pos] {
        b'+' => parse_simple(buf, pos),
        b'-' => parse_error(buf, pos),
        b':' => parse_integer(buf, pos),
        b'$' => parse_bulk(buf, pos),
        b'*' => parse_array(buf, pos),
        _ => bail!("unknown RESP type byte: {:02x}", buf[pos]),
    }
}

fn find_crlf(buf: &[u8], start: usize) -> Option<usize> {
    buf[start..].windows(2).position(|w| w == b"\r\n").map(|i| start + i)
}

fn parse_line(buf: &[u8], pos: usize) -> Option<(&[u8], usize)> {
    find_crlf(buf, pos).map(|end| (&buf[pos..end], end + 2))
}

fn parse_simple(buf: &[u8], pos: usize) -> Result<Option<(RespValue, usize)>> {
    match parse_line(buf, pos + 1) {
        Some((line, next)) => Ok(Some((RespValue::Simple(String::from_utf8_lossy(line).to_string()), next))),
        None => Ok(None),
    }
}

fn parse_error(buf: &[u8], pos: usize) -> Result<Option<(RespValue, usize)>> {
    match parse_line(buf, pos + 1) {
        Some((line, next)) => Ok(Some((RespValue::Error(String::from_utf8_lossy(line).to_string()), next))),
        None => Ok(None),
    }
}

fn parse_integer(buf: &[u8], pos: usize) -> Result<Option<(RespValue, usize)>> {
    match parse_line(buf, pos + 1) {
        Some((line, next)) => {
            let s = std::str::from_utf8(line)?;
            Ok(Some((RespValue::Integer(s.parse()?), next)))
        }
        None => Ok(None),
    }
}

fn parse_bulk(buf: &[u8], pos: usize) -> Result<Option<(RespValue, usize)>> {
    let Some((line, next)) = parse_line(buf, pos + 1) else { return Ok(None) };
    let len: i64 = std::str::from_utf8(line)?.parse()?;
    if len < 0 {
        return Ok(Some((RespValue::Bulk(None), next)));
    }
    let len = len as usize;
    let end = next + len + 2; // data + \r\n
    if buf.len() < end {
        return Ok(None);
    }
    Ok(Some((RespValue::Bulk(Some(buf[next..next + len].to_vec())), end)))
}

fn parse_array(buf: &[u8], pos: usize) -> Result<Option<(RespValue, usize)>> {
    let Some((line, mut next)) = parse_line(buf, pos + 1) else { return Ok(None) };
    let count: i64 = std::str::from_utf8(line)?.parse()?;
    if count < 0 {
        return Ok(Some((RespValue::Array(None), next)));
    }
    let mut items = Vec::with_capacity(count as usize);
    for _ in 0..count {
        match parse_value(buf, next)? {
            Some((val, consumed)) => {
                items.push(val);
                next = consumed;
            }
            None => return Ok(None),
        }
    }
    Ok(Some((RespValue::Array(Some(items)), next)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple() {
        let input = b"+OK\r\n";
        let (val, n) = parse_resp(input).unwrap().unwrap();
        assert_eq!(val, RespValue::Simple("OK".into()));
        assert_eq!(n, 5);
    }

    #[test]
    fn test_parse_array_command() {
        // *3\r\n$3\r\nSET\r\n$3\r\nkey\r\n$5\r\nvalue\r\n
        let input = b"*3\r\n$3\r\nSET\r\n$3\r\nkey\r\n$5\r\nvalue\r\n";
        let (val, _) = parse_resp(input).unwrap().unwrap();
        assert_eq!(val.to_command_string(), "SET key value");
    }

    // ─── Edge case tests ─────────────────────────────────────────────────

    #[test]
    fn test_incomplete_simple_string() {
        assert!(parse_resp(b"+OK").unwrap().is_none());
        assert!(parse_resp(b"+").unwrap().is_none());
        assert!(parse_resp(b"").unwrap().is_none());
    }

    #[test]
    fn test_incomplete_bulk_string() {
        // Header complete, data incomplete
        assert!(parse_resp(b"$5\r\nhel").unwrap().is_none());
        assert!(parse_resp(b"$5\r\n").unwrap().is_none());
        assert!(parse_resp(b"$").unwrap().is_none());
    }

    #[test]
    fn test_incomplete_array() {
        // Array header but missing elements
        assert!(parse_resp(b"*3\r\n$3\r\nSET\r\n").unwrap().is_none());
        assert!(parse_resp(b"*3\r\n").unwrap().is_none());
    }

    #[test]
    fn test_null_bulk_string() {
        let (val, n) = parse_resp(b"$-1\r\n").unwrap().unwrap();
        assert_eq!(val, RespValue::Bulk(None));
        assert_eq!(n, 5);
    }

    #[test]
    fn test_null_array() {
        let (val, n) = parse_resp(b"*-1\r\n").unwrap().unwrap();
        assert_eq!(val, RespValue::Array(None));
        assert_eq!(n, 5);
    }

    #[test]
    fn test_empty_array() {
        let (val, n) = parse_resp(b"*0\r\n").unwrap().unwrap();
        assert_eq!(val, RespValue::Array(Some(vec![])));
        assert_eq!(n, 4);
    }

    #[test]
    fn test_error_response() {
        let (val, _) = parse_resp(b"-ERR unknown command\r\n").unwrap().unwrap();
        assert!(matches!(val, RespValue::Error(_)));
        assert_eq!(val.to_command_string(), "ERR: ERR unknown command");
    }

    #[test]
    fn test_integer_response() {
        let (val, n) = parse_resp(b":42\r\n").unwrap().unwrap();
        assert_eq!(val, RespValue::Integer(42));
        assert_eq!(n, 5);
    }

    #[test]
    fn test_nested_array() {
        // Array containing an array
        let input = b"*2\r\n*2\r\n$1\r\na\r\n$1\r\nb\r\n$1\r\nc\r\n";
        let (val, _) = parse_resp(input).unwrap().unwrap();
        assert!(matches!(val, RespValue::Array(Some(_))));
    }

    #[test]
    fn test_pipeline_multiple_commands() {
        // Two commands in one buffer
        let input = b"*1\r\n$4\r\nPING\r\n*2\r\n$3\r\nGET\r\n$3\r\nkey\r\n";
        let (val1, n1) = parse_resp(input).unwrap().unwrap();
        assert_eq!(val1.to_command_string(), "PING");
        let (val2, _) = parse_resp(&input[n1..]).unwrap().unwrap();
        assert_eq!(val2.to_command_string(), "GET key");
    }

    #[test]
    fn test_empty_bulk_string() {
        let (val, n) = parse_resp(b"$0\r\n\r\n").unwrap().unwrap();
        assert_eq!(val, RespValue::Bulk(Some(vec![])));
        assert_eq!(n, 6);
    }

    #[test]
    fn test_malformed_type_byte() {
        assert!(parse_resp(b"Xgarbage\r\n").is_err());
    }

    #[test]
    fn test_partial_bytes_consumed() {
        // Extra data after valid response
        let input = b"+OK\r\n+EXTRA\r\n";
        let (val, n) = parse_resp(input).unwrap().unwrap();
        assert_eq!(val, RespValue::Simple("OK".into()));
        assert_eq!(n, 5);
        // Should be able to parse the second response
        let (val2, _) = parse_resp(&input[n..]).unwrap().unwrap();
        assert_eq!(val2, RespValue::Simple("EXTRA".into()));
    }
}
