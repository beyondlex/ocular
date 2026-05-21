use anyhow::{Result, bail};

/// Redis RESP 值
#[derive(Debug, Clone, PartialEq)]
pub enum RespValue {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(Option<Vec<u8>>),
    Array(Option<Vec<RespValue>>),
}

impl RespValue {
    /// 将 RESP 命令格式化为可读字符串，如 "SET key value"
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

/// 从字节流中解析一个完整的 RESP 值，返回 (解析结果, 消耗的字节数)
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
}
