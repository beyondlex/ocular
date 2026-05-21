/// MySQL wire protocol parser (client command packets)
///
/// MySQL packet format: [3-byte length][1-byte seq][payload]
/// Command byte is the first byte of payload.

/// Parsed MySQL packet
#[derive(Debug, Clone)]
pub struct MysqlPacket {
    pub command: MysqlCommand,
    pub payload: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MysqlCommand {
    Query,
    StmtPrepare,
    StmtExecute,
    StmtClose,
    Ping,
    Quit,
    InitDb,
    FieldList,
    Other(u8),
}

/// MySQL response type (simplified)
#[derive(Debug, Clone)]
pub enum MysqlResponse {
    Ok { affected_rows: u64, message: String },
    Error { code: u16, message: String },
    ResultSet { columns: Vec<String>, rows: Vec<Vec<String>>, total_rows: usize },
    Other,
}

/// Parse a MySQL client command packet. Returns summary string if parseable.
/// Only returns Some for actual command packets (seq=0, known command byte).
pub fn parse_mysql_request(buf: &[u8]) -> Option<MysqlPacket> {
    // Need at least 4-byte header + 1-byte command
    if buf.len() < 5 {
        return None;
    }
    let payload_len = (buf[0] as usize) | (buf[1] as usize) << 8 | (buf[2] as usize) << 16;
    let seq = buf[3];
    // Command packets always start a new sequence (seq=0)
    if seq != 0 {
        return None;
    }
    if buf.len() < 4 + payload_len || payload_len == 0 {
        return None;
    }
    let cmd_byte = buf[4];
    let data = &buf[5..4 + payload_len];

    let (command, payload) = match cmd_byte {
        0x03 => (MysqlCommand::Query, String::from_utf8_lossy(data).to_string()),
        0x16 => (MysqlCommand::StmtPrepare, String::from_utf8_lossy(data).to_string()),
        0x17 => {
            let stmt_id = if data.len() >= 4 {
                u32::from_le_bytes([data[0], data[1], data[2], data[3]])
            } else { 0 };
            (MysqlCommand::StmtExecute, format!("stmt_id={}", stmt_id))
        }
        0x19 => {
            let stmt_id = if data.len() >= 4 {
                u32::from_le_bytes([data[0], data[1], data[2], data[3]])
            } else { 0 };
            (MysqlCommand::StmtClose, format!("stmt_id={}", stmt_id))
        }
        0x0e => {
            // COM_PING: payload should be exactly 1 byte (just the command)
            if payload_len != 1 { return None; }
            (MysqlCommand::Ping, "PING".to_string())
        }
        0x01 => {
            // COM_QUIT: payload should be exactly 1 byte
            if payload_len != 1 { return None; }
            (MysqlCommand::Quit, "QUIT".to_string())
        }
        0x02 => (MysqlCommand::InitDb, String::from_utf8_lossy(data).to_string()),
        // 0x04 COM_FIELD_LIST: auto-completion noise from mysql CLI, skip
        0x04 => return None,
        // Unknown command bytes during handshake — skip them
        _ => return None,
    };

    Some(MysqlPacket { command, payload })
}

/// Parse a MySQL server response packet (first packet of response).
/// Only parses responses to commands (seq >= 1).
pub fn parse_mysql_response(buf: &[u8]) -> Option<MysqlResponse> {
    if buf.len() < 5 {
        return None;
    }
    let payload_len = (buf[0] as usize) | (buf[1] as usize) << 8 | (buf[2] as usize) << 16;
    let seq = buf[3];
    // Response to a command has seq >= 1 (server increments from client's seq=0)
    if seq == 0 {
        return None;
    }
    if buf.len() < 4 + payload_len || payload_len == 0 {
        return None;
    }
    let marker = buf[4];
    match marker {
        0x00 => {
            // OK packet
            let affected = read_lenenc(&buf[5..]).unwrap_or(0);
            Some(MysqlResponse::Ok {
                affected_rows: affected,
                message: format!("OK ({} rows affected)", affected),
            })
        }
        0xff => {
            // ERR packet
            let code = if buf.len() >= 7 {
                u16::from_le_bytes([buf[5], buf[6]])
            } else { 0 };
            // Skip sql_state marker (#) and 5-byte state
            let msg_start = if buf.len() > 13 && buf[7] == b'#' { 13 } else { 7 };
            let msg = String::from_utf8_lossy(&buf[msg_start..4 + payload_len]).to_string();
            Some(MysqlResponse::Error { code, message: format!("ERR {} {}", code, msg) })
        }
        _ => {
            // Result set: first byte is column count
            let col_count = marker as usize;
            let (columns, rows) = parse_resultset_packets(buf, col_count);
            let total_rows = rows.len();
            Some(MysqlResponse::ResultSet { columns, rows, total_rows })
        }
    }
}

impl MysqlPacket {
    pub fn to_summary(&self) -> String {
        let cmd = match self.command {
            MysqlCommand::Query => "QUERY",
            MysqlCommand::StmtPrepare => "PREPARE",
            MysqlCommand::StmtExecute => "EXECUTE",
            MysqlCommand::StmtClose => "STMT_CLOSE",
            MysqlCommand::Ping => "PING",
            MysqlCommand::Quit => "QUIT",
            MysqlCommand::InitDb => "USE",
            MysqlCommand::FieldList => "FIELD_LIST",
            MysqlCommand::Other(c) => return format!("CMD(0x{:02x})", c),
        };
        if self.payload.is_empty() || self.payload == cmd {
            cmd.to_string()
        } else {
            // Truncate long queries for summary
            let truncated: String = self.payload.chars().take(120).collect();
            if truncated.len() < self.payload.len() {
                format!("{} {}...", cmd, truncated)
            } else {
                format!("{} {}", cmd, self.payload)
            }
        }
    }
}

impl MysqlResponse {
    pub fn to_summary(&self) -> String {
        match self {
            MysqlResponse::Ok { message, .. } => message.clone(),
            MysqlResponse::Error { message, .. } => message.clone(),
            MysqlResponse::ResultSet { total_rows, columns, .. } => {
                format!("ResultSet ({} rows, {} cols: {})", total_rows, columns.len(),
                    columns.iter().take(5).cloned().collect::<Vec<_>>().join(", "))
            }
            MysqlResponse::Other => "...".to_string(),
        }
    }

    /// Formatted display for detail panel
    pub fn to_display(&self) -> String {
        match self {
            MysqlResponse::Ok { message, .. } => message.clone(),
            MysqlResponse::Error { message, .. } => message.clone(),
            MysqlResponse::ResultSet { columns, rows, total_rows } => {
                let mut out = format!("ResultSet: {} rows\n", total_rows);
                if !columns.is_empty() {
                    out.push_str(&format!("Columns: {}\n", columns.join(" | ")));
                    out.push_str(&"-".repeat(60));
                    out.push('\n');
                }
                for row in rows.iter().take(20) {
                    out.push_str(&row.join(" | "));
                    out.push('\n');
                }
                if *total_rows > 20 {
                    out.push_str(&format!("... ({} more rows)\n", total_rows - 20));
                }
                out
            }
            MysqlResponse::Other => "...".to_string(),
        }
    }
}

/// Read a length-encoded integer (simplified, handles 1-byte case)
fn read_lenenc(buf: &[u8]) -> Option<u64> {
    if buf.is_empty() { return None; }
    match buf[0] {
        n if n < 0xfb => Some(n as u64),
        0xfc if buf.len() >= 3 => Some(u16::from_le_bytes([buf[1], buf[2]]) as u64),
        0xfd if buf.len() >= 4 => Some((buf[1] as u64) | (buf[2] as u64) << 8 | (buf[3] as u64) << 16),
        0xfe if buf.len() >= 9 => Some(u64::from_le_bytes([buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7], buf[8]])),
        _ => Some(0),
    }
}

/// Returns (bytes_consumed, value)
fn read_lenenc_with_size(buf: &[u8]) -> Option<(usize, u64)> {
    if buf.is_empty() { return None; }
    match buf[0] {
        n if n < 0xfb => Some((1, n as u64)),
        0xfc if buf.len() >= 3 => Some((3, u16::from_le_bytes([buf[1], buf[2]]) as u64)),
        0xfd if buf.len() >= 4 => Some((4, (buf[1] as u64) | (buf[2] as u64) << 8 | (buf[3] as u64) << 16)),
        0xfe if buf.len() >= 9 => Some((9, u64::from_le_bytes([buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7], buf[8]]))),
        _ => None,
    }
}

/// Read a length-encoded string from buffer, returns (bytes_consumed, string)
fn read_lenenc_str(buf: &[u8]) -> Option<(usize, String)> {
    if buf.is_empty() { return None; }
    if buf[0] == 0xfb {
        return Some((1, "NULL".to_string()));
    }
    let (hdr_size, len) = read_lenenc_with_size(buf)?;
    let len = len as usize;
    if buf.len() < hdr_size + len { return None; }
    let s = String::from_utf8_lossy(&buf[hdr_size..hdr_size + len]).to_string();
    Some((hdr_size + len, s))
}

/// Skip a MySQL packet at `pos` in buffer, return next position
fn skip_packet(buf: &[u8], pos: usize) -> Option<usize> {
    if pos + 4 > buf.len() { return None; }
    let pkt_len = (buf[pos] as usize) | (buf[pos+1] as usize) << 8 | (buf[pos+2] as usize) << 16;
    let end = pos + 4 + pkt_len;
    if end > buf.len() { None } else { Some(end) }
}

/// Parse a ResultSet from the full TCP buffer.
/// Extracts column names and row values.
fn parse_resultset_packets(buf: &[u8], col_count: usize) -> (Vec<String>, Vec<Vec<String>>) {
    let mut columns = Vec::new();
    let mut rows = Vec::new();

    // Skip the first packet (column count packet, already parsed)
    let Some(mut pos) = skip_packet(buf, 0) else { return (columns, rows) };

    // Read column definition packets
    for _ in 0..col_count {
        if pos + 4 >= buf.len() { break; }
        let pkt_len = (buf[pos] as usize) | (buf[pos+1] as usize) << 8 | (buf[pos+2] as usize) << 16;
        let payload_start = pos + 4;
        let payload_end = payload_start + pkt_len;
        if payload_end > buf.len() { break; }
        let payload = &buf[payload_start..payload_end];
        // Column def: catalog(lenenc_str), schema, table, org_table, name, ...
        // We want the 5th lenenc_str (name)
        let mut p = 0;
        for i in 0..5 {
            if let Some((consumed, s)) = read_lenenc_str(&payload[p..]) {
                if i == 4 { columns.push(s); }
                p += consumed;
            } else { break; }
        }
        pos = payload_end;
    }

    // Skip EOF packet (if present, marker 0xfe)
    if pos + 4 < buf.len() {
        let pkt_len = (buf[pos] as usize) | (buf[pos+1] as usize) << 8 | (buf[pos+2] as usize) << 16;
        let marker = if pos + 4 < buf.len() { buf[pos + 4] } else { 0 };
        if marker == 0xfe && pkt_len < 9 {
            pos = pos + 4 + pkt_len;
        }
    }

    // Read row packets (text protocol: each field is a lenenc_str)
    let max_rows = 10000; // parse all, truncate at display time
    loop {
        if pos + 4 >= buf.len() { break; }
        let pkt_len = (buf[pos] as usize) | (buf[pos+1] as usize) << 8 | (buf[pos+2] as usize) << 16;
        let payload_start = pos + 4;
        let payload_end = payload_start + pkt_len;
        if payload_end > buf.len() { break; }
        let marker = buf[payload_start];
        // EOF or OK packet signals end
        if (marker == 0xfe && pkt_len < 9) || marker == 0x00 { break; }
        // ERR packet
        if marker == 0xff { break; }

        if rows.len() < max_rows {
            let payload = &buf[payload_start..payload_end];
            let mut row = Vec::new();
            let mut p = 0;
            for _ in 0..col_count {
                if let Some((consumed, s)) = read_lenenc_str(&payload[p..]) {
                    row.push(s);
                    p += consumed;
                } else { break; }
            }
            rows.push(row);
        }
        pos = payload_end;
        if rows.len() >= max_rows { break; }
    }

    (columns, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_query() {
        // COM_QUERY "SELECT 1"
        let sql = b"SELECT 1";
        let mut pkt = vec![
            (sql.len() + 1) as u8, 0, 0, // 3-byte length
            0,                             // sequence
            0x03,                          // COM_QUERY
        ];
        pkt.extend_from_slice(sql);
        let result = parse_mysql_request(&pkt).unwrap();
        assert_eq!(result.command, MysqlCommand::Query);
        assert_eq!(result.to_summary(), "QUERY SELECT 1");
    }

    #[test]
    fn test_parse_ok_response() {
        // OK packet: 0 affected rows
        let pkt = vec![7, 0, 0, 1, 0x00, 0, 0, 0x02, 0, 0, 0];
        let resp = parse_mysql_response(&pkt).unwrap();
        assert!(matches!(resp, MysqlResponse::Ok { .. }));
    }
}
