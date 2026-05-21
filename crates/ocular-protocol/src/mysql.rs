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
    ResultSet,
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
        0x04 => (MysqlCommand::FieldList, String::from_utf8_lossy(data).to_string()),
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
            // Result set (column count as first byte)
            Some(MysqlResponse::ResultSet)
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
            MysqlResponse::ResultSet => "ResultSet".to_string(),
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
