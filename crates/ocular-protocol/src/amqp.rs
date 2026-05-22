/// AMQP 0-9-1 wire protocol parser
///
/// Frame format: [type:1][channel:2][size:4][payload...][frame_end:0xCE]
/// Method frame payload: [class_id:2][method_id:2][arguments...]

const FRAME_METHOD: u8 = 1;
const FRAME_HEADER: u8 = 2;
const FRAME_BODY: u8 = 3;
const FRAME_HEARTBEAT: u8 = 8;
const FRAME_END: u8 = 0xCE;

#[derive(Debug, Clone)]
pub struct AmqpFrame {
    pub frame_type: u8,
    pub channel: u16,
    pub method: Option<AmqpMethod>,
    pub body: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct AmqpMethod {
    pub class_id: u16,
    pub method_id: u16,
    pub summary: String,
    pub detail: String,
}

/// Parse the first complete AMQP frame from buffer. Returns None if incomplete.
pub fn parse_amqp_frame(buf: &[u8]) -> Option<AmqpFrame> {
    // Protocol header (client sends "AMQP\x00\x00\x09\x01")
    if buf.len() >= 8 && &buf[0..4] == b"AMQP" {
        return Some(AmqpFrame {
            frame_type: 0,
            channel: 0,
            method: Some(AmqpMethod {
                class_id: 0,
                method_id: 0,
                summary: "AMQP Protocol Header".into(),
                detail: format!("AMQP {}.{}.{}.{}", buf[4], buf[5], buf[6], buf[7]),
            }),
            body: None,
        });
    }

    if buf.len() < 8 { return None; }
    let frame_type = buf[0];
    let channel = u16::from_be_bytes([buf[1], buf[2]]);
    let size = u32::from_be_bytes([buf[3], buf[4], buf[5], buf[6]]) as usize;
    let total = 7 + size + 1;
    if buf.len() < total { return None; }
    if buf[total - 1] != FRAME_END { return None; }

    let payload = &buf[7..7 + size];

    match frame_type {
        FRAME_METHOD => {
            if size < 4 { return None; }
            let class_id = u16::from_be_bytes([payload[0], payload[1]]);
            let method_id = u16::from_be_bytes([payload[2], payload[3]]);
            let args = &payload[4..];
            let (summary, detail) = decode_method(class_id, method_id, args);
            Some(AmqpFrame {
                frame_type,
                channel,
                method: Some(AmqpMethod { class_id, method_id, summary, detail }),
                body: None,
            })
        }
        FRAME_HEADER => Some(AmqpFrame {
            frame_type, channel, method: None,
            body: Some(payload.to_vec()),
        }),
        FRAME_BODY => Some(AmqpFrame {
            frame_type, channel, method: None,
            body: Some(payload.to_vec()),
        }),
        FRAME_HEARTBEAT => Some(AmqpFrame {
            frame_type, channel,
            method: Some(AmqpMethod {
                class_id: 0, method_id: 0,
                summary: "Heartbeat".into(),
                detail: "Heartbeat".into(),
            }),
            body: None,
        }),
        _ => None,
    }
}

/// Returns the byte length of the first frame in buf, or None if incomplete.
fn frame_len(buf: &[u8]) -> Option<usize> {
    if buf.len() >= 8 && &buf[0..4] == b"AMQP" {
        return Some(8);
    }
    if buf.len() < 7 { return None; }
    let size = u32::from_be_bytes([buf[3], buf[4], buf[5], buf[6]]) as usize;
    let total = 7 + size + 1;
    if buf.len() < total { return None; }
    Some(total)
}

/// Returns true if this method is "fire-and-forget" (server sends no response).
/// Basic.Publish (60,40), Basic.Ack (60,80), Basic.Reject (60,90), Basic.Nack (60,120)
pub fn is_async_method(class_id: u16, method_id: u16) -> bool {
    matches!((class_id, method_id),
        (60, 40) | (60, 50) | (60, 60) | (60, 80) | (60, 90) | (60, 120))
}

/// Parse a client→server buffer that may contain multiple frames.
/// For publish: Method + Header + Body. Returns (summary, detail_with_body).
pub fn parse_amqp_request_full(buf: &[u8]) -> Option<(String, String)> {
    let frame = parse_amqp_frame(buf)?;
    let method = frame.method.as_ref()?;
    let summary = method.summary.clone();
    let mut detail = method.detail.clone();

    // Try to extract body from subsequent frames in the same buffer
    if let Some(first_len) = frame_len(buf) {
        let mut pos = first_len;
        while let Some(flen) = frame_len(&buf[pos..]) {
            if let Some(f) = parse_amqp_frame(&buf[pos..]) {
                if f.frame_type == FRAME_BODY {
                    if let Some(body) = &f.body {
                        let text = String::from_utf8_lossy(body);
                        detail = format!("{}\nBody: {}", detail, text);
                    }
                }
            }
            pos += flen;
        }
    }

    Some((summary, detail))
}

/// Parse a client→server buffer, returning a summary for the event list
pub fn parse_amqp_request(buf: &[u8]) -> Option<String> {
    let frame = parse_amqp_frame(buf)?;
    frame.method.map(|m| m.summary)
}

/// Parse a server→client buffer, returning a summary
pub fn parse_amqp_response(buf: &[u8]) -> Option<String> {
    let frame = parse_amqp_frame(buf)?;
    match frame.frame_type {
        FRAME_BODY => {
            let body = frame.body.unwrap_or_default();
            let text = String::from_utf8_lossy(&body);
            let truncated: String = text.chars().take(80).collect();
            Some(format!("Body: {}", truncated))
        }
        _ => frame.method.map(|m| m.summary),
    }
}

/// Parse response with full detail for the detail panel
pub fn format_amqp_response_detail(buf: &[u8]) -> Option<String> {
    let frame = parse_amqp_frame(buf)?;
    match frame.frame_type {
        FRAME_BODY => {
            let body = frame.body.unwrap_or_default();
            Some(String::from_utf8_lossy(&body).to_string())
        }
        _ => frame.method.map(|m| m.detail),
    }
}

/// Read a short string: [len:1][data...]
fn read_short_str(buf: &[u8]) -> Option<(String, usize)> {
    if buf.is_empty() { return None; }
    let len = buf[0] as usize;
    if buf.len() < 1 + len { return None; }
    Some((String::from_utf8_lossy(&buf[1..1 + len]).to_string(), 1 + len))
}

fn decode_method(class_id: u16, method_id: u16, args: &[u8]) -> (String, String) {
    match (class_id, method_id) {
        // Connection
        (10, 10) => ("Connection.Start".into(), "Connection.Start".into()),
        (10, 11) => ("Connection.StartOk".into(), "Connection.StartOk".into()),
        (10, 30) => ("Connection.Tune".into(), format_tune(args)),
        (10, 31) => ("Connection.TuneOk".into(), format_tune(args)),
        (10, 40) => {
            let vhost = read_short_str(args).map(|(s, _)| s).unwrap_or_default();
            (format!("Connection.Open vhost={}", vhost), format!("Connection.Open vhost={}", vhost))
        }
        (10, 41) => ("Connection.OpenOk".into(), "Connection.OpenOk".into()),
        (10, 50) => ("Connection.Close".into(), format_close(args)),
        (10, 51) => ("Connection.CloseOk".into(), "Connection.CloseOk".into()),
        // Channel
        (20, 10) => ("Channel.Open".into(), "Channel.Open".into()),
        (20, 11) => ("Channel.OpenOk".into(), "Channel.OpenOk".into()),
        (20, 40) => ("Channel.Close".into(), format_close(args)),
        (20, 41) => ("Channel.CloseOk".into(), "Channel.CloseOk".into()),
        // Exchange
        (40, 10) => {
            let (exchange, detail) = parse_exchange_declare(args);
            (format!("Exchange.Declare {}", exchange), detail)
        }
        (40, 11) => ("Exchange.DeclareOk".into(), "Exchange.DeclareOk".into()),
        (40, 20) => ("Exchange.Delete".into(), "Exchange.Delete".into()),
        (40, 21) => ("Exchange.DeleteOk".into(), "Exchange.DeleteOk".into()),
        // Queue
        (50, 10) => {
            let (queue, detail) = parse_queue_declare(args);
            (format!("Queue.Declare {}", queue), detail)
        }
        (50, 11) => parse_queue_declare_ok(args),
        (50, 20) => {
            let (detail_str, summary) = parse_queue_bind(args);
            (summary, detail_str)
        }
        (50, 21) => ("Queue.BindOk".into(), "Queue.BindOk".into()),
        (50, 30) => ("Queue.Purge".into(), "Queue.Purge".into()),
        (50, 31) => ("Queue.PurgeOk".into(), "Queue.PurgeOk".into()),
        (50, 40) => ("Queue.Delete".into(), "Queue.Delete".into()),
        (50, 41) => ("Queue.DeleteOk".into(), "Queue.DeleteOk".into()),
        // Basic
        (60, 10) => ("Basic.Qos".into(), "Basic.Qos".into()),
        (60, 11) => ("Basic.QosOk".into(), "Basic.QosOk".into()),
        (60, 20) => {
            let (summary, detail) = parse_basic_consume(args);
            (summary, detail)
        }
        (60, 21) => ("Basic.ConsumeOk".into(), "Basic.ConsumeOk".into()),
        (60, 30) => ("Basic.Cancel".into(), "Basic.Cancel".into()),
        (60, 31) => ("Basic.CancelOk".into(), "Basic.CancelOk".into()),
        (60, 40) => {
            let (summary, detail) = parse_basic_publish(args);
            (summary, detail)
        }
        (60, 50) => ("Basic.Return".into(), "Basic.Return".into()),
        (60, 60) => {
            let (summary, detail) = parse_basic_deliver(args);
            (summary, detail)
        }
        (60, 70) => ("Basic.Get".into(), parse_basic_get(args)),
        (60, 71) => ("Basic.GetOk".into(), "Basic.GetOk".into()),
        (60, 72) => ("Basic.GetEmpty".into(), "Basic.GetEmpty".into()),
        (60, 80) => ("Basic.Ack".into(), "Basic.Ack".into()),
        (60, 90) => ("Basic.Reject".into(), "Basic.Reject".into()),
        (60, 120) => ("Basic.Nack".into(), "Basic.Nack".into()),
        // Confirm
        (85, 10) => ("Confirm.Select".into(), "Confirm.Select".into()),
        (85, 11) => ("Confirm.SelectOk".into(), "Confirm.SelectOk".into()),
        // Tx
        (90, 10) => ("Tx.Select".into(), "Tx.Select".into()),
        (90, 11) => ("Tx.SelectOk".into(), "Tx.SelectOk".into()),
        (90, 20) => ("Tx.Commit".into(), "Tx.Commit".into()),
        (90, 21) => ("Tx.CommitOk".into(), "Tx.CommitOk".into()),
        (90, 30) => ("Tx.Rollback".into(), "Tx.Rollback".into()),
        (90, 31) => ("Tx.RollbackOk".into(), "Tx.RollbackOk".into()),
        _ => {
            let s = format!("Method({}.{})", class_id, method_id);
            (s.clone(), s)
        }
    }
}

fn format_tune(args: &[u8]) -> String {
    if args.len() < 8 { return "Connection.Tune".into(); }
    let channel_max = u16::from_be_bytes([args[0], args[1]]);
    let frame_max = u32::from_be_bytes([args[2], args[3], args[4], args[5]]);
    let heartbeat = u16::from_be_bytes([args[6], args[7]]);
    format!("channel_max={} frame_max={} heartbeat={}", channel_max, frame_max, heartbeat)
}

fn format_close(args: &[u8]) -> String {
    if args.len() < 4 { return "Close".into(); }
    let code = u16::from_be_bytes([args[0], args[1]]);
    let reason = read_short_str(&args[2..]).map(|(s, _)| s).unwrap_or_default();
    format!("code={} reason={}", code, reason)
}

fn parse_exchange_declare(args: &[u8]) -> (String, String) {
    // [reserved:2][exchange:short_str][type:short_str][flags:1][table]
    if args.len() < 3 { return (String::new(), "Exchange.Declare".into()); }
    let rest = &args[2..]; // skip reserved
    let (exchange, consumed) = read_short_str(rest).unwrap_or_default();
    let rest = &rest[consumed..];
    let (ex_type, _) = read_short_str(rest).unwrap_or_default();
    let detail = format!("Exchange.Declare exchange={} type={}", exchange, ex_type);
    (exchange, detail)
}

fn parse_queue_declare(args: &[u8]) -> (String, String) {
    // [reserved:2][queue:short_str][flags:1][table]
    if args.len() < 3 { return (String::new(), "Queue.Declare".into()); }
    let rest = &args[2..];
    let (queue, _) = read_short_str(rest).unwrap_or_default();
    let detail = format!("Queue.Declare queue={}", queue);
    (queue, detail)
}

fn parse_queue_declare_ok(args: &[u8]) -> (String, String) {
    let (queue, consumed) = read_short_str(args).unwrap_or_default();
    let rest = &args[consumed..];
    let (msg_count, consumer_count) = if rest.len() >= 8 {
        (u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]),
         u32::from_be_bytes([rest[4], rest[5], rest[6], rest[7]]))
    } else { (0, 0) };
    let summary = format!("Queue.DeclareOk {}", queue);
    let detail = format!("Queue.DeclareOk queue={} messages={} consumers={}", queue, msg_count, consumer_count);
    (summary, detail)
}

fn parse_queue_bind(args: &[u8]) -> (String, String) {
    // [reserved:2][queue:short_str][exchange:short_str][routing_key:short_str]...
    if args.len() < 3 { return ("Queue.Bind".into(), "Queue.Bind".into()); }
    let rest = &args[2..];
    let (queue, c1) = read_short_str(rest).unwrap_or_default();
    let rest = &rest[c1..];
    let (exchange, c2) = read_short_str(rest).unwrap_or_default();
    let rest = &rest[c2..];
    let (routing_key, _) = read_short_str(rest).unwrap_or_default();
    let summary = format!("Queue.Bind {} → {} ({})", queue, exchange, routing_key);
    let detail = format!("Queue.Bind queue={} exchange={} routing_key={}", queue, exchange, routing_key);
    (detail, summary)
}

fn parse_basic_publish(args: &[u8]) -> (String, String) {
    // [reserved:2][exchange:short_str][routing_key:short_str][flags:1]
    if args.len() < 3 { return ("Basic.Publish".into(), "Basic.Publish".into()); }
    let rest = &args[2..];
    let (exchange, c1) = read_short_str(rest).unwrap_or_default();
    let rest = &rest[c1..];
    let (routing_key, _) = read_short_str(rest).unwrap_or_default();
    let ex = if exchange.is_empty() { "(default)" } else { &exchange };
    let summary = format!("Basic.Publish → {} key={}", ex, routing_key);
    let detail = format!("Basic.Publish exchange={} routing_key={}", ex, routing_key);
    (summary, detail)
}

fn parse_basic_deliver(args: &[u8]) -> (String, String) {
    // [consumer_tag:short_str][delivery_tag:8][redelivered:1][exchange:short_str][routing_key:short_str]
    let (consumer_tag, c1) = read_short_str(args).unwrap_or_default();
    let rest = &args[c1..];
    if rest.len() < 9 { return ("Basic.Deliver".into(), "Basic.Deliver".into()); }
    let delivery_tag = u64::from_be_bytes([rest[0], rest[1], rest[2], rest[3], rest[4], rest[5], rest[6], rest[7]]);
    let rest = &rest[9..]; // skip delivery_tag + redelivered
    let (exchange, c2) = read_short_str(rest).unwrap_or_default();
    let rest = &rest[c2..];
    let (routing_key, _) = read_short_str(rest).unwrap_or_default();
    let summary = format!("Basic.Deliver key={}", routing_key);
    let detail = format!("Basic.Deliver consumer={} delivery_tag={} exchange={} routing_key={}", consumer_tag, delivery_tag, exchange, routing_key);
    (summary, detail)
}

fn parse_basic_consume(args: &[u8]) -> (String, String) {
    // [reserved:2][queue:short_str][consumer_tag:short_str]...
    if args.len() < 3 { return ("Basic.Consume".into(), "Basic.Consume".into()); }
    let rest = &args[2..];
    let (queue, c1) = read_short_str(rest).unwrap_or_default();
    let rest = &rest[c1..];
    let (consumer_tag, _) = read_short_str(rest).unwrap_or_default();
    let summary = format!("Basic.Consume {}", queue);
    let detail = format!("Basic.Consume queue={} consumer_tag={}", queue, consumer_tag);
    (summary, detail)
}

fn parse_basic_get(args: &[u8]) -> String {
    // [reserved:2][queue:short_str][no_ack:1]
    if args.len() < 3 { return "Basic.Get".into(); }
    let rest = &args[2..];
    let (queue, _) = read_short_str(rest).unwrap_or_default();
    format!("Basic.Get queue={}", queue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_protocol_header() {
        let buf = b"AMQP\x00\x00\x09\x01";
        let frame = parse_amqp_frame(buf).unwrap();
        assert_eq!(frame.method.unwrap().summary, "AMQP Protocol Header");
    }

    #[test]
    fn test_parse_heartbeat() {
        // Heartbeat frame: type=8, channel=0, size=0, frame_end=0xCE
        let buf = [8, 0, 0, 0, 0, 0, 0, 0xCE];
        let frame = parse_amqp_frame(&buf).unwrap();
        assert_eq!(frame.frame_type, FRAME_HEARTBEAT);
        assert_eq!(frame.method.unwrap().summary, "Heartbeat");
    }

    #[test]
    fn test_parse_basic_publish_frame() {
        // Method frame: type=1, channel=1, class=60, method=40
        // args: reserved(2) + exchange "test" + routing_key "rk"
        let mut buf = Vec::new();
        buf.push(1); // type
        buf.extend_from_slice(&1u16.to_be_bytes()); // channel
        let args: Vec<u8> = vec![
            0, 0, // reserved
            4, b't', b'e', b's', b't', // exchange
            2, b'r', b'k', // routing_key
            0, // flags
        ];
        let payload_len = 4 + args.len();
        buf.extend_from_slice(&(payload_len as u32).to_be_bytes()); // size
        buf.extend_from_slice(&60u16.to_be_bytes()); // class
        buf.extend_from_slice(&40u16.to_be_bytes()); // method
        buf.extend_from_slice(&args);
        buf.push(0xCE);

        let frame = parse_amqp_frame(&buf).unwrap();
        let method = frame.method.unwrap();
        assert!(method.summary.contains("Basic.Publish"));
        assert!(method.summary.contains("test"));
        assert!(method.summary.contains("rk"));
    }
}
