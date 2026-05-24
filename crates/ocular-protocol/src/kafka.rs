// Kafka binary protocol parser
// Wire format: [length:4][api_key:2][api_version:2][correlation_id:4][client_id...]...
// Response:    [length:4][correlation_id:4]...

/// Kafka API keys
fn api_key_name(key: i16) -> &'static str {
    match key {
        0 => "Produce",
        1 => "Fetch",
        2 => "ListOffsets",
        3 => "Metadata",
        4 => "LeaderAndIsr",
        5 => "StopReplica",
        6 => "UpdateMetadata",
        7 => "ControlledShutdown",
        8 => "OffsetCommit",
        9 => "OffsetFetch",
        10 => "FindCoordinator",
        11 => "JoinGroup",
        12 => "Heartbeat",
        13 => "LeaveGroup",
        14 => "SyncGroup",
        15 => "DescribeGroups",
        16 => "ListGroups",
        18 => "ApiVersions",
        19 => "CreateTopics",
        20 => "DeleteTopics",
        21 => "DeleteRecords",
        22 => "InitProducerId",
        23 => "OffsetForLeaderEpoch",
        24 => "AddPartitionsToTxn",
        25 => "AddOffsetsToTxn",
        26 => "EndTxn",
        31 => "DescribeAcls",
        32 => "DescribeConfigs",
        33 => "AlterConfigs",
        35 => "DescribeLogDirs",
        36 => "SaslHandshake",
        37 => "SaslAuthenticate",
        42 => "DeleteGroups",
        44 => "IncrementalAlterConfigs",
        46 => "DescribeProducers",
        47 => "OffsetDelete",
        50 => "DescribeCluster",
        60 => "DescribeTopicPartitions",
        75 => "DescribeTopicPartitions",
        _ => "Unknown",
    }
}

/// Parse Kafka request: extract api_key, version, correlation_id, client_id, and topic if present
pub fn parse_kafka_request(buf: &[u8]) -> Option<String> {
    if buf.len() < 12 { return None; }
    let length = i32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if buf.len() < 4 + length { return None; }

    let api_key = i16::from_be_bytes([buf[4], buf[5]]);
    let api_version = i16::from_be_bytes([buf[6], buf[7]]);
    let name = api_key_name(api_key);

    // Try to extract topic for Produce/Fetch
    let detail = match api_key {
        0 => extract_produce_topic(buf).map(|t| format!("Produce v{} topic={}", api_version, t)),
        1 => extract_fetch_topic(buf).map(|t| format!("Fetch v{} topic={}", api_version, t)),
        3 => Some(format!("Metadata v{}", api_version)),
        18 => Some(format!("ApiVersions v{}", api_version)),
        19 => extract_topic_after_client_id(buf).map(|t| format!("CreateTopics v{} topic={}", api_version, t)),
        20 => extract_topic_after_client_id(buf).map(|t| format!("DeleteTopics v{} topic={}", api_version, t)),
        _ if name == "Unknown" => Some(format!("ApiKey({}) v{}", api_key, api_version)),
        _ => None,
    };

    Some(detail.unwrap_or_else(|| format!("{} v{}", name, api_version)))
}

/// Parse Kafka response summary
pub fn parse_kafka_response(buf: &[u8]) -> Option<String> {
    if buf.len() < 8 { return None; }
    // Response is just [length:4][correlation_id:4][...payload]
    // We can't determine much without knowing the request api_key
    let length = i32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let payload_size = length.saturating_sub(4);
    Some(format!("OK ({} bytes)", payload_size))
}

/// Format response detail
pub fn format_kafka_response_detail(buf: &[u8]) -> Option<String> {
    parse_kafka_response(buf)
}

/// Extract full command with body for Produce requests
pub fn extract_kafka_full_command(buf: &[u8]) -> Option<String> {
    if buf.len() < 12 { return None; }
    let api_key = i16::from_be_bytes([buf[4], buf[5]]);
    let summary = parse_kafka_request(buf)?;

    // Only extract body for Produce (api_key=0)
    if api_key != 0 {
        return Some(summary);
    }

    // Try structured parsing first, fall back to string extraction
    if let Some(records) = extract_produce_records(buf) {
        if !records.is_empty() {
            return Some(format!("{}\n{}", summary, records.join("\n")));
        }
    }

    // Fallback: find readable strings that look like message content
    if let Some(body) = extract_readable_payload(buf) {
        return Some(format!("{}\n{}", summary, body));
    }

    Some(summary)
}

/// Find readable UTF-8 strings in the payload that look like message content
fn extract_readable_payload(buf: &[u8]) -> Option<String> {
    // Skip the first ~40 bytes (header + topic metadata)
    // Look for JSON-like or substantial text content
    let search_start = 40.min(buf.len());
    let mut best: Option<&[u8]> = None;

    let mut i = search_start;
    while i < buf.len() {
        // Find start of a printable sequence
        if buf[i] >= 0x20 && buf[i] < 0x7F {
            let start = i;
            while i < buf.len() && buf[i] >= 0x20 && buf[i] < 0x7F {
                i += 1;
            }
            let candidate = &buf[start..i];
            let len = candidate.len();
            if len < 10 { continue; }

            // Look for JSON start within the candidate
            if let Some(json_offset) = candidate.iter().position(|&b| b == b'{' || b == b'[') {
                let json_slice = &candidate[json_offset..];
                if json_slice.len() >= 5 {
                    return Some(String::from_utf8_lossy(json_slice).to_string());
                }
            }

            // Keep the longest string as fallback
            if best.is_none_or(|b| candidate.len() > b.len()) {
                best = Some(candidate);
            }
        } else {
            i += 1;
        }
    }

    best.map(|b| String::from_utf8_lossy(b).to_string())
}

/// Extract record values from a Produce request by finding the RecordBatch
/// RecordBatch layout (after the batch starts):
///   baseOffset(8) + batchLength(4) + partitionLeaderEpoch(4) + magic(1=0x02)
///   + crc(4) + attributes(2) + lastOffsetDelta(4) + baseTimestamp(8)
///   + maxTimestamp(8) + producerId(8) + producerEpoch(2) + baseSequence(4)
///   + recordCount(4) + [records...]
fn extract_produce_records(buf: &[u8]) -> Option<Vec<String>> {
    // Find magic byte 0x02 at the correct position within a RecordBatch
    // The magic byte is at offset 16 from the start of a RecordBatch
    // Validate by checking batchLength makes sense
    let mut results = Vec::new();

    for pos in 12..buf.len().saturating_sub(61) {
        // magic byte check at offset +16
        if buf[pos + 16] != 0x02 { continue; }

        // Validate batchLength (at offset +8, 4 bytes)
        let batch_len = i32::from_be_bytes([buf[pos + 8], buf[pos + 9], buf[pos + 10], buf[pos + 11]]);
        if !(49..=10_000_000).contains(&batch_len) { continue; }
        let batch_len = batch_len as usize;
        if pos + 12 + batch_len > buf.len() { continue; }

        // Validate baseOffset should be 0 for producer requests
        let base_offset = i64::from_be_bytes([buf[pos], buf[pos+1], buf[pos+2], buf[pos+3], buf[pos+4], buf[pos+5], buf[pos+6], buf[pos+7]]);
        if base_offset != 0 { continue; }

        // attributes at offset +21 from batch start (after magic + crc)
        // Actually: baseOffset(8) + batchLength(4) + partitionLeaderEpoch(4) + magic(1) + crc(4) = 21
        let attributes = i16::from_be_bytes([buf[pos + 21], buf[pos + 22]]);
        let compression = attributes & 0x07;

        // recordCount at offset +45 from batch start
        let record_count = i32::from_be_bytes([buf[pos + 53], buf[pos + 54], buf[pos + 55], buf[pos + 56]]);
        if !(0..=100_000).contains(&record_count) { continue; }

        if compression != 0 {
            results.push(format!("({} record(s), compressed)", record_count));
            return Some(results);
        }

        // Records start at offset +57 from batch start
        let records_start = pos + 57;
        let records_end = pos + 12 + batch_len;
        let mut rpos = records_start;

        for _ in 0..record_count {
            if rpos >= records_end { break; }
            if let Some((value, consumed)) = parse_record(buf, rpos, records_end) {
                if let Some(v) = value {
                    results.push(v);
                }
                rpos += consumed;
            } else {
                break;
            }
        }

        return if results.is_empty() { None } else { Some(results) };
    }

    None
}

/// Parse a single Record from a RecordBatch (uncompressed)
/// Record: length(varint) + attributes(1) + timestampDelta(varint) + offsetDelta(varint)
///         + keyLength(varint) + key + valueLength(varint) + value + headersCount(varint) + ...
fn parse_record(buf: &[u8], start: usize, end: usize) -> Option<(Option<String>, usize)> {
    let mut pos = start;
    if pos >= end { return None; }

    // record length (varint)
    let (record_len, n) = decode_varint(buf, pos)?;
    pos += n;
    let record_len = record_len as usize;
    if pos + record_len > end { return None; }
    let record_end = pos + record_len;

    // attributes (1 byte)
    if pos >= record_end { return Some((None, (record_end - start))); }
    pos += 1;

    // timestampDelta (varint)
    let (_, n) = decode_varint(buf, pos)?;
    pos += n;

    // offsetDelta (varint)
    let (_, n) = decode_varint(buf, pos)?;
    pos += n;

    // keyLength (varint) — -1 means null
    let (key_len, n) = decode_varint(buf, pos)?;
    pos += n;
    if key_len > 0 {
        pos += key_len as usize;
    }

    // valueLength (varint)
    let (value_len, n) = decode_varint(buf, pos)?;
    pos += n;

    let value = if value_len > 0 && pos + value_len as usize <= record_end {
        let v = &buf[pos..pos + value_len as usize];
        Some(String::from_utf8_lossy(v).to_string())
    } else {
        None
    };

    Some((value, record_end - start))
}

/// Decode a zigzag-encoded varint
fn decode_varint(buf: &[u8], start: usize) -> Option<(i64, usize)> {
    let mut result: u64 = 0;
    let mut shift = 0;
    let mut pos = start;
    loop {
        if pos >= buf.len() { return None; }
        let byte = buf[pos];
        result |= ((byte & 0x7F) as u64) << shift;
        pos += 1;
        if byte & 0x80 == 0 { break; }
        shift += 7;
        if shift > 63 { return None; }
    }
    // zigzag decode
    let decoded = ((result >> 1) as i64) ^ -((result & 1) as i64);
    Some((decoded, pos - start))
}

/// Check if a Kafka request frame is complete (length-prefixed)
pub fn kafka_frame_complete(buf: &[u8]) -> bool {
    if buf.len() < 4 { return false; }
    let length = i32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    buf.len() >= 4 + length
}

/// Try to extract topic name from Produce request
/// Format after header: [transactional_id][acks:2][timeout:4][topic_count:4][topic_name...]
/// Simplified: scan for topic string after client_id
fn extract_produce_topic(buf: &[u8]) -> Option<String> {
    extract_topic_after_client_id(buf)
}

/// Try to extract topic name from Fetch request
fn extract_fetch_topic(buf: &[u8]) -> Option<String> {
    extract_topic_after_client_id(buf)
}

/// Skip client_id and look for a topic-like string in the payload
fn extract_topic_after_client_id(buf: &[u8]) -> Option<String> {
    if buf.len() < 14 { return None; }
    // Skip: length(4) + api_key(2) + version(2) + correlation_id(4) = 12
    let pos = 12;
    // client_id is a nullable string: [len:2][bytes...]
    if pos + 2 > buf.len() { return None; }
    let client_id_len = i16::from_be_bytes([buf[pos], buf[pos + 1]]);
    let after_client = if client_id_len < 0 {
        pos + 2
    } else {
        pos + 2 + client_id_len as usize
    };

    // Scan remaining bytes for a short string that looks like a topic name
    // This is a heuristic — Kafka's format varies by api_key and version
    find_first_string(buf, after_client)
}

/// Find the first length-prefixed string in buf starting at pos
fn find_first_string(buf: &[u8], pos: usize) -> Option<String> {
    if pos + 2 > buf.len() { return None; }
    // Try different offsets to find a reasonable string
    for offset in 0..20 {
        let p = pos + offset;
        if p + 2 > buf.len() { break; }
        let len = i16::from_be_bytes([buf[p], buf[p + 1]]);
        if len > 0 && len < 256 && p + 2 + len as usize <= buf.len() {
            let s = &buf[p + 2..p + 2 + len as usize];
            if let Ok(topic) = std::str::from_utf8(s) {
                if topic.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.') && topic.len() > 1 {
                    return Some(topic.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(api_key: i16, api_version: i16, client_id: &str) -> Vec<u8> {
        let client_id_bytes = client_id.as_bytes();
        let payload_len = 2 + 2 + 4 + 2 + client_id_bytes.len();
        let mut buf = Vec::new();
        buf.extend_from_slice(&(payload_len as i32).to_be_bytes()); // length
        buf.extend_from_slice(&api_key.to_be_bytes());
        buf.extend_from_slice(&api_version.to_be_bytes());
        buf.extend_from_slice(&1i32.to_be_bytes()); // correlation_id
        buf.extend_from_slice(&(client_id_bytes.len() as i16).to_be_bytes());
        buf.extend_from_slice(client_id_bytes);
        buf
    }

    #[test]
    fn test_parse_metadata_request() {
        let buf = make_request(3, 12, "my-app");
        assert_eq!(parse_kafka_request(&buf), Some("Metadata v12".into()));
    }

    #[test]
    fn test_parse_api_versions() {
        let buf = make_request(18, 3, "kafka-client");
        assert_eq!(parse_kafka_request(&buf), Some("ApiVersions v3".into()));
    }

    #[test]
    fn test_parse_response() {
        let mut buf = vec![0, 0, 0, 20]; // length = 20
        buf.extend_from_slice(&1i32.to_be_bytes()); // correlation_id
        buf.extend_from_slice(&[0u8; 16]); // payload
        assert_eq!(parse_kafka_response(&buf), Some("OK (16 bytes)".into()));
    }

    #[test]
    fn test_frame_complete() {
        let mut buf = vec![0, 0, 0, 4]; // length = 4
        buf.extend_from_slice(&[0u8; 4]);
        assert!(kafka_frame_complete(&buf));
        assert!(!kafka_frame_complete(&buf[..6]));
    }

    #[test]
    fn test_frame_incomplete() {
        assert!(!kafka_frame_complete(&[0, 0, 0, 10, 0, 0]));
    }
}
