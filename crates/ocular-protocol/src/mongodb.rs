//! MongoDB wire protocol parser (OP_MSG only, modern MongoDB 3.6+)
//! All integers are little-endian.

const OP_MSG: i32 = 2013;
const OP_COMPRESSED: i32 = 2012;

/// Get the total message length from a MongoDB wire protocol header.
/// Returns None if buffer is too small or length is invalid.
pub fn mongo_msg_len(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 { return None; }
    let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if !(16..=48 * 1024 * 1024).contains(&len) { return None; }
    Some(len)
}

/// Parse a MongoDB request (client→server), returning a command summary.
pub fn parse_mongo_request(buf: &[u8]) -> Option<String> {
    let doc = extract_body_doc(buf)?;
    let cmd = first_key(&doc)?;
    let db = get_string_field(&doc, "$db").unwrap_or_default();
    let detail = match cmd.as_str() {
        "find" => {
            let coll = get_string_field(&doc, "find").unwrap_or_default();
            let filter = get_doc_field_summary(&doc, "filter");
            format!("find {}.{} {}", db, coll, filter)
        }
        "insert" => {
            let coll = get_string_field(&doc, "insert").unwrap_or_default();
            let n = get_array_len(&doc, "documents");
            format!("insert {}.{} ({} docs)", db, coll, n)
        }
        "update" => {
            let coll = get_string_field(&doc, "update").unwrap_or_default();
            let n = get_array_len(&doc, "updates");
            format!("update {}.{} ({} ops)", db, coll, n)
        }
        "delete" => {
            let coll = get_string_field(&doc, "delete").unwrap_or_default();
            let n = get_array_len(&doc, "deletes");
            format!("delete {}.{} ({} ops)", db, coll, n)
        }
        "aggregate" => {
            let coll = get_string_field(&doc, "aggregate").unwrap_or_default();
            format!("aggregate {}.{}", db, coll)
        }
        "getMore" => {
            let coll = get_string_field(&doc, "collection").unwrap_or_default();
            format!("getMore {}.{}", db, coll)
        }
        _ => {
            if db.is_empty() { cmd.clone() } else { format!("{} {}", cmd, db) }
        }
    };
    Some(detail)
}

/// Extract full command detail (for Detail panel) — mongosh-style replayable statements.
pub fn extract_mongo_full_command(buf: &[u8]) -> Option<String> {
    let doc = extract_body_doc(buf)?;
    let cmd = first_key(&doc)?;
    let db = get_string_field(&doc, "$db").unwrap_or_default();
    match cmd.as_str() {
        "find" => {
            let coll = get_string_field(&doc, "find").unwrap_or_default();
            let filter = get_doc_field_summary(&doc, "filter");
            let limit = get_i32_field(&doc, "limit");
            let sort = get_raw_doc_field(&doc, "sort").map(|d| bson_doc_to_json_like(&d));
            let mut s = format!("db.{}.find({})", coll, filter);
            if let Some(sort_str) = sort { s.push_str(&format!(".sort({})", sort_str)); }
            if let Some(l) = limit { s.push_str(&format!(".limit({})", l)); }
            Some(s)
        }
        "insert" => {
            let coll = get_string_field(&doc, "insert").unwrap_or_default();
            let docs = get_array_docs(&doc, "documents");
            if docs.len() == 1 {
                Some(format!("db.{}.insertOne({})", coll, bson_doc_to_json_like(&docs[0])))
            } else {
                let items: Vec<String> = docs.iter().take(10).map(|d| bson_doc_to_json_like(d)).collect();
                let mut s = format!("db.{}.insertMany([{}])", coll, items.join(", "));
                if docs.len() > 10 { s.push_str(&format!(" // +{} more", docs.len() - 10)); }
                Some(s)
            }
        }
        "update" => {
            let coll = get_string_field(&doc, "update").unwrap_or_default();
            let updates = get_array_docs(&doc, "updates");
            if updates.len() == 1 {
                let q = get_doc_field_summary(&updates[0], "q");
                let u = get_doc_field_summary(&updates[0], "u");
                let multi = get_i32_field(&updates[0], "multi").unwrap_or(0) != 0
                    || has_field(&updates[0], "multi") && get_f64_field(&updates[0], "multi") == Some(1.0);
                let method = if multi { "updateMany" } else { "updateOne" };
                Some(format!("db.{}.{}({}, {})", coll, method, q, u))
            } else {
                Some(format!("db.{}.bulkWrite([...{} ops])", coll, updates.len()))
            }
        }
        "delete" => {
            let coll = get_string_field(&doc, "delete").unwrap_or_default();
            let deletes = get_array_docs(&doc, "deletes");
            if deletes.len() == 1 {
                let q = get_doc_field_summary(&deletes[0], "q");
                let limit = get_i32_field(&deletes[0], "limit").unwrap_or(0);
                let method = if limit == 1 { "deleteOne" } else { "deleteMany" };
                Some(format!("db.{}.{}({})", coll, method, q))
            } else {
                Some(format!("db.{}.bulkWrite([...{} ops])", coll, deletes.len()))
            }
        }
        "aggregate" => {
            let coll = get_string_field(&doc, "aggregate").unwrap_or_default();
            Some(format!("db.{}.aggregate([...])", coll))
        }
        "findAndModify" => {
            let coll = get_string_field(&doc, "findAndModify").unwrap_or_default();
            let query = get_doc_field_summary(&doc, "query");
            let update = get_doc_field_summary(&doc, "update");
            Some(format!("db.{}.findOneAndUpdate({}, {})", coll, query, update))
        }
        "count" | "countDocuments" => {
            let coll = get_string_field(&doc, &cmd).unwrap_or_default();
            let query = get_doc_field_summary(&doc, "query");
            Some(format!("db.{}.countDocuments({})", coll, query))
        }
        _ => {
            if db.is_empty() { Some(cmd) } else { Some(format!("{} {}", cmd, db)) }
        }
    }
}

/// Parse a MongoDB response (server→client), returning a summary.
pub fn parse_mongo_response(buf: &[u8]) -> Option<String> {
    let doc = extract_body_doc(buf)?;
    let ok = get_f64_field(&doc, "ok");
    if ok == Some(0.0) {
        let errmsg = get_string_field(&doc, "errmsg").unwrap_or("error".into());
        let code = get_i32_field(&doc, "code").map(|c| format!(" ({})", c)).unwrap_or_default();
        return Some(format!("ERR{} {}", code, errmsg));
    }
    // Check for cursor result
    if let Some(cursor_doc) = get_raw_doc_field(&doc, "cursor") {
        let batch_key = if has_field(&cursor_doc, "firstBatch") { "firstBatch" } else { "nextBatch" };
        let n = get_array_len(&cursor_doc, batch_key);
        return Some(format!("OK ({} docs)", n));
    }
    // Check for n (insert/update/delete result)
    if let Some(n) = get_i32_field(&doc, "n") {
        let modified = get_i32_field(&doc, "nModified");
        if let Some(m) = modified {
            return Some(format!("OK (n={}, modified={})", n, m));
        }
        return Some(format!("OK (n={})", n));
    }
    Some("OK".into())
}

/// Format detailed response for the detail panel.
pub fn format_mongo_response_detail(buf: &[u8]) -> Option<String> {
    let doc = extract_body_doc(buf)?;
    let ok = get_f64_field(&doc, "ok");
    if ok == Some(0.0) {
        let errmsg = get_string_field(&doc, "errmsg").unwrap_or("error".into());
        let code = get_i32_field(&doc, "code").unwrap_or(0);
        let codename = get_string_field(&doc, "codeName").unwrap_or_default();
        return Some(format!("ERROR {} ({}): {}", code, codename, errmsg));
    }
    if let Some(cursor_doc) = get_raw_doc_field(&doc, "cursor") {
        let batch_key = if has_field(&cursor_doc, "firstBatch") { "firstBatch" } else { "nextBatch" };
        let docs = get_array_docs(&cursor_doc, batch_key);
        let mut lines = Vec::new();
        lines.push(format!("{} documents:", docs.len()));
        for (i, d) in docs.iter().enumerate().take(20) {
            lines.push(format!("  [{}] {}", i, bson_doc_to_json_like(d)));
        }
        if docs.len() > 20 {
            lines.push(format!("  ... ({} more)", docs.len() - 20));
        }
        return Some(lines.join("\n"));
    }
    parse_mongo_response(buf)
}

// --- Internal helpers ---

/// Extract the Kind 0 body BSON document from an OP_MSG.
fn extract_body_doc(buf: &[u8]) -> Option<Vec<u8>> {
    if buf.len() < 21 { return None; } // header(16) + flags(4) + kind(1)
    let opcode = i32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);
    if opcode != OP_MSG && opcode != OP_COMPRESSED { return None; }
    if opcode == OP_COMPRESSED { return decompress_op_compressed(buf); }
    // flags at offset 16, sections start at offset 20
    let mut pos = 20;
    while pos < buf.len() {
        let kind = buf[pos];
        pos += 1;
        if kind == 0 {
            // Kind 0: single BSON document
            if pos + 4 > buf.len() { return None; }
            let doc_len = i32::from_le_bytes([buf[pos], buf[pos+1], buf[pos+2], buf[pos+3]]) as usize;
            if pos + doc_len > buf.len() { return None; }
            return Some(buf[pos..pos+doc_len].to_vec());
        } else if kind == 1 {
            // Kind 1: document sequence, skip
            if pos + 4 > buf.len() { return None; }
            let sec_len = i32::from_le_bytes([buf[pos], buf[pos+1], buf[pos+2], buf[pos+3]]) as usize;
            pos += sec_len;
        } else {
            break;
        }
    }
    None
}

/// Decompress an OP_COMPRESSED message and extract the body doc from the inner OP_MSG.
fn decompress_op_compressed(buf: &[u8]) -> Option<Vec<u8>> {
    if buf.len() < 25 { return None; }
    let original_opcode = i32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]);
    if original_opcode != OP_MSG { return None; }
    let uncompressed_size = i32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]) as usize;
    let compressor_id = buf[24];
    let compressed = &buf[25..];

    let decompressed = match compressor_id {
        0 => compressed.to_vec(),
        1 => snap::raw::Decoder::new().decompress_vec(compressed).ok()?,
        2 => {
            use std::io::Read;
            let mut decoder = flate2::read::ZlibDecoder::new(compressed);
            let mut out = Vec::with_capacity(uncompressed_size);
            decoder.read_to_end(&mut out).ok()?;
            out
        }
        3 => zstd::decode_all(compressed).ok()?,
        _ => return None,
    };

    // decompressed = flags(4) + sections... (OP_MSG body without 16-byte header)
    if decompressed.len() < 5 { return None; }
    let mut pos = 4; // skip flags
    while pos < decompressed.len() {
        let kind = decompressed[pos];
        pos += 1;
        if kind == 0 {
            if pos + 4 > decompressed.len() { return None; }
            let doc_len = i32::from_le_bytes([decompressed[pos], decompressed[pos+1], decompressed[pos+2], decompressed[pos+3]]) as usize;
            if pos + doc_len > decompressed.len() { return None; }
            return Some(decompressed[pos..pos+doc_len].to_vec());
        } else if kind == 1 {
            if pos + 4 > decompressed.len() { return None; }
            let sec_len = i32::from_le_bytes([decompressed[pos], decompressed[pos+1], decompressed[pos+2], decompressed[pos+3]]) as usize;
            pos += sec_len;
        } else {
            break;
        }
    }
    None
}

/// Get the first key name from a BSON document (the command name).
fn first_key(doc: &[u8]) -> Option<String> {
    if doc.len() < 6 { return None; }
    // doc[0..4] = size, doc[4] = element type, doc[5..] = cstring key
    let key = read_cstr(&doc[5..])?;
    Some(key)
}

/// Read a null-terminated C string.
fn read_cstr(buf: &[u8]) -> Option<String> {
    let end = buf.iter().position(|&b| b == 0)?;
    Some(String::from_utf8_lossy(&buf[..end]).to_string())
}

// ─── BSON field iterator ────────────────────────────────────────────────────

/// Iterates over fields in a BSON document, yielding `(type, key, value_pos)`
/// for each element. Centralizes the skip-value logic that was previously
/// duplicated across get_string_field, get_f64_field, get_i32_field,
/// get_raw_doc_field, and has_field.
struct BsonIter<'a> {
    doc: &'a [u8],
    pos: usize,
}

impl<'a> BsonIter<'a> {
    fn new(doc: &'a [u8]) -> Self {
        Self { doc, pos: 4 } // skip 4-byte document size
    }
}

impl<'a> Iterator for BsonIter<'a> {
    type Item = (u8, &'a str, usize);

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.doc.len().saturating_sub(1) { return None; }
        let etype = self.doc[self.pos];
        if etype == 0 { return None; }
        self.pos += 1;
        // Read key as &str (no allocation)
        let key_end = self.doc[self.pos..].iter().position(|&b| b == 0)?;
        let key = std::str::from_utf8(&self.doc[self.pos..self.pos + key_end]).ok()?;
        self.pos += key_end + 1;
        let value_pos = self.pos;
        if !bson_skip_value(self.doc, etype, &mut self.pos) {
            return None; // unknown type or out-of-bounds — bail
        }
        Some((etype, key, value_pos))
    }
}

/// Advance `pos` past a BSON value of the given type. Returns false on error.
fn bson_skip_value(doc: &[u8], etype: u8, pos: &mut usize) -> bool {
    match etype {
        0x01 => { *pos += 8; }                              // double
        0x02 => {                                            // string: len(4) + data
            if *pos + 4 > doc.len() { return false; }
            let slen = i32::from_le_bytes([doc[*pos], doc[*pos+1], doc[*pos+2], doc[*pos+3]]) as usize;
            *pos += 4 + slen;
        }
        0x03 | 0x04 => {                                     // document / array: self-describing length
            if *pos + 4 > doc.len() { return false; }
            let dlen = i32::from_le_bytes([doc[*pos], doc[*pos+1], doc[*pos+2], doc[*pos+3]]) as usize;
            *pos += dlen;
        }
        0x05 => {                                            // binary: len(4) + subtype(1) + data
            if *pos + 4 > doc.len() { return false; }
            let blen = i32::from_le_bytes([doc[*pos], doc[*pos+1], doc[*pos+2], doc[*pos+3]]) as usize;
            *pos += 5 + blen;
        }
        0x07 => { *pos += 12; }                              // ObjectId
        0x08 => { *pos += 1; }                               // boolean
        0x09 | 0x11 | 0x12 => { *pos += 8; }                 // datetime, timestamp, int64
        0x0A => {}                                            // null
        0x10 => { *pos += 4; }                               // int32
        0x13 => { *pos += 16; }                              // decimal128
        _ => { return false; }                                // unknown type
    }
    true
}

fn get_string_field(doc: &[u8], name: &str) -> Option<String> {
    for (etype, key, pos) in BsonIter::new(doc) {
        if key == name && etype == 0x02 {
            let slen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize;
            return Some(String::from_utf8_lossy(&doc[pos+4..pos+4+slen.saturating_sub(1)]).to_string());
        }
    }
    None
}

fn get_f64_field(doc: &[u8], name: &str) -> Option<f64> {
    for (etype, key, pos) in BsonIter::new(doc) {
        if key == name {
            match etype {
                0x01 => return Some(f64::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3], doc[pos+4], doc[pos+5], doc[pos+6], doc[pos+7]])),
                0x10 => {
                    let v = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]);
                    return Some(v as f64);
                }
                _ => {}
            }
        }
    }
    None
}

fn get_i32_field(doc: &[u8], name: &str) -> Option<i32> {
    for (etype, key, pos) in BsonIter::new(doc) {
        if key == name && etype == 0x10 {
            return Some(i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]));
        }
    }
    None
}

fn get_raw_doc_field(doc: &[u8], name: &str) -> Option<Vec<u8>> {
    for (etype, key, pos) in BsonIter::new(doc) {
        if key == name && (etype == 0x03 || etype == 0x04) {
            let dlen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize;
            return Some(doc[pos..pos+dlen].to_vec());
        }
    }
    None
}

fn has_field(doc: &[u8], name: &str) -> bool {
    BsonIter::new(doc).any(|(_, key, _)| key == name)
}

fn get_array_len(doc: &[u8], name: &str) -> usize {
    let Some(arr) = get_raw_doc_field(doc, name) else { return 0 };
    BsonIter::new(&arr).count()
}

fn get_array_docs(doc: &[u8], name: &str) -> Vec<Vec<u8>> {
    let Some(arr) = get_raw_doc_field(doc, name) else { return vec![] };
    let mut docs = Vec::new();
    for (etype, _key, pos) in BsonIter::new(&arr) {
        if etype != 0x03 { continue; }
        let dlen = i32::from_le_bytes([arr[pos], arr[pos+1], arr[pos+2], arr[pos+3]]) as usize;
        if pos + dlen <= arr.len() {
            docs.push(arr[pos..pos+dlen].to_vec());
        }
    }
    docs
}

fn get_doc_field_summary(doc: &[u8], name: &str) -> String {
    let Some(subdoc) = get_raw_doc_field(doc, name) else { return "{}".into() };
    bson_doc_to_json_like(&subdoc)
}

/// Simple BSON doc to JSON-like string (for display, not full fidelity).
fn bson_doc_to_json_like(doc: &[u8]) -> String {
    let mut parts = Vec::new();
    let mut pos = 4;
    while pos < doc.len().saturating_sub(1) {
        let etype = doc[pos];
        if etype == 0 { break; }
        pos += 1;
        let Some(key) = read_cstr(&doc[pos..]) else { break };
        pos += key.len() + 1;
        let val = match etype {
            0x01 => { let v = if pos + 8 <= doc.len() { f64::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3], doc[pos+4], doc[pos+5], doc[pos+6], doc[pos+7]]) } else { 0.0 }; pos += 8; format!("{}", v) }
            0x02 => { if pos + 4 > doc.len() { break; } let slen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize; pos += 4; let s = String::from_utf8_lossy(&doc[pos..pos+slen.saturating_sub(1)]).to_string(); pos += slen; format!("\"{}\"", s) }
            0x03 => { if pos + 4 > doc.len() { break; } let dlen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize; let s = bson_doc_to_json_like(&doc[pos..pos+dlen]); pos += dlen; s }
            0x04 => { if pos + 4 > doc.len() { break; } let dlen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize; pos += dlen; "[...]".into() }
            0x07 => { pos += 12; "ObjectId(...)".into() }
            0x08 => { let v = doc[pos] != 0; pos += 1; format!("{}", v) }
            0x09 => { pos += 8; "Date(...)".into() }
            0x0A => { "null".into() }
            0x10 => { let v = if pos + 4 <= doc.len() { i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) } else { 0 }; pos += 4; format!("{}", v) }
            0x12 => { let v = if pos + 8 <= doc.len() { i64::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3], doc[pos+4], doc[pos+5], doc[pos+6], doc[pos+7]]) } else { 0 }; pos += 8; format!("{}", v) }
            _ => { break; }
        };
        if key == "_id" || key == "lsid" { continue; }
        parts.push(format!("{}: {}", key, val));
        if parts.len() >= 8 { parts.push("...".into()); break; }
    }
    format!("{{{}}}", parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal OP_MSG with a Kind 0 BSON body document.
    fn build_op_msg(doc: &[u8]) -> Vec<u8> {
        let msg_len = 16 + 4 + 1 + doc.len(); // header + flags + kind + doc
        let mut buf = Vec::new();
        buf.extend_from_slice(&(msg_len as i32).to_le_bytes()); // messageLength
        buf.extend_from_slice(&1i32.to_le_bytes()); // requestID
        buf.extend_from_slice(&0i32.to_le_bytes()); // responseTo
        buf.extend_from_slice(&OP_MSG.to_le_bytes()); // opCode
        buf.extend_from_slice(&0u32.to_le_bytes()); // flagBits
        buf.push(0); // kind 0
        buf.extend_from_slice(doc);
        buf
    }

    /// Build a simple BSON document: {"cmd": "coll", "$db": "testdb"}
    fn build_simple_cmd(cmd: &str, coll: &str) -> Vec<u8> {
        let mut doc = Vec::new();
        doc.extend_from_slice(&[0; 4]); // placeholder for size
        // cmd: coll (string)
        doc.push(0x02); // string type
        doc.extend_from_slice(cmd.as_bytes());
        doc.push(0);
        let val = format!("{}\0", coll);
        doc.extend_from_slice(&(val.len() as i32).to_le_bytes());
        doc.extend_from_slice(val.as_bytes());
        // $db: "testdb" (string)
        doc.push(0x02);
        doc.extend_from_slice(b"$db\0");
        let db = "testdb\0";
        doc.extend_from_slice(&(db.len() as i32).to_le_bytes());
        doc.extend_from_slice(db.as_bytes());
        // end
        doc.push(0);
        let len = doc.len() as i32;
        doc[0..4].copy_from_slice(&len.to_le_bytes());
        doc
    }

    #[test]
    fn test_parse_find_request() {
        let doc = build_simple_cmd("find", "users");
        let buf = build_op_msg(&doc);
        let result = parse_mongo_request(&buf).unwrap();
        assert!(result.contains("find"));
        assert!(result.contains("testdb"));
        assert!(result.contains("users"));
    }

    #[test]
    fn test_parse_insert_request() {
        let doc = build_simple_cmd("insert", "users");
        let buf = build_op_msg(&doc);
        let result = parse_mongo_request(&buf).unwrap();
        assert!(result.contains("insert"));
        assert!(result.contains("testdb.users"));
    }

    #[test]
    fn test_parse_response_ok() {
        // {"ok": 1.0}
        let mut doc = Vec::new();
        doc.extend_from_slice(&[0; 4]);
        doc.push(0x01); // double
        doc.extend_from_slice(b"ok\0");
        doc.extend_from_slice(&1.0f64.to_le_bytes());
        doc.push(0);
        let len = doc.len() as i32;
        doc[0..4].copy_from_slice(&len.to_le_bytes());

        let buf = build_op_msg(&doc);
        let result = parse_mongo_response(&buf).unwrap();
        assert_eq!(result, "OK");
    }

    #[test]
    fn test_parse_response_error() {
        // {"ok": 0.0, "errmsg": "not found", "code": 26}
        let mut doc = Vec::new();
        doc.extend_from_slice(&[0; 4]);
        // ok: 0.0
        doc.push(0x01);
        doc.extend_from_slice(b"ok\0");
        doc.extend_from_slice(&0.0f64.to_le_bytes());
        // errmsg: "not found"
        doc.push(0x02);
        doc.extend_from_slice(b"errmsg\0");
        let msg = "not found\0";
        doc.extend_from_slice(&(msg.len() as i32).to_le_bytes());
        doc.extend_from_slice(msg.as_bytes());
        // code: 26
        doc.push(0x10);
        doc.extend_from_slice(b"code\0");
        doc.extend_from_slice(&26i32.to_le_bytes());
        doc.push(0);
        let len = doc.len() as i32;
        doc[0..4].copy_from_slice(&len.to_le_bytes());

        let buf = build_op_msg(&doc);
        let result = parse_mongo_response(&buf).unwrap();
        assert!(result.contains("ERR"));
        assert!(result.contains("26"));
        assert!(result.contains("not found"));
    }

    #[test]
    fn test_mongo_msg_len() {
        let buf = build_op_msg(&build_simple_cmd("ping", "admin"));
        assert_eq!(mongo_msg_len(&buf), Some(buf.len()));
    }

    #[test]
    fn test_mongo_msg_len_too_short() {
        assert_eq!(mongo_msg_len(&[1, 2, 3]), None);
    }

    #[test]
    fn test_extract_full_command_find() {
        let doc = build_simple_cmd("find", "users");
        let buf = build_op_msg(&doc);
        let result = extract_mongo_full_command(&buf).unwrap();
        assert!(result.contains("db.users.find"));
    }

    // ─── Edge case tests ─────────────────────────────────────────────────

    #[test]
    fn test_buffer_too_short() {
        assert!(parse_mongo_request(&[]).is_none());
        assert!(parse_mongo_request(&[0u8; 10]).is_none());
        assert!(parse_mongo_request(&[0u8; 20]).is_none()); // header but no doc
    }

    #[test]
    fn test_invalid_opcode() {
        // OP_MSG = 2013, use something else
        let mut buf = build_op_msg(&build_simple_cmd("find", "users"));
        // Overwrite opcode at offset 12
        buf[12] = 0x00;
        buf[13] = 0x00;
        buf[14] = 0x00;
        buf[15] = 0x00;
        assert!(parse_mongo_request(&buf).is_none());
    }

    #[test]
    fn test_empty_document() {
        // OP_MSG with empty BSON doc (just size + terminator)
        let doc = vec![5, 0, 0, 0, 0]; // 5-byte empty doc
        let buf = build_op_msg(&doc);
        assert!(parse_mongo_request(&buf).is_none()); // no command key
    }

    #[test]
    fn test_truncated_bson_document() {
        // Doc claims to be 100 bytes but only has 10
        let mut doc = vec![100, 0, 0, 0]; // lies about size
        doc.extend_from_slice(&[0x02]); // string type
        doc.extend_from_slice(b"cmd\0"); // key
        // Missing value — truncated
        let buf = build_op_msg(&doc);
        assert!(parse_mongo_request(&buf).is_none());
    }

    #[test]
    fn test_mongo_msg_len_bounds() {
        // Too small (< 16)
        assert_eq!(mongo_msg_len(&[15, 0, 0, 0]), None);
        // Too large (> 48MB)
        assert_eq!(mongo_msg_len(&[0xFF, 0xFF, 0xFF, 0x03]), None);
    }

    #[test]
    fn test_response_ok_with_n() {
        // {"ok": 1.0, "n": 5}
        let mut doc = Vec::new();
        doc.extend_from_slice(&[0; 4]); // size placeholder
        doc.push(0x01); // double
        doc.extend_from_slice(b"ok\0");
        doc.extend_from_slice(&1.0f64.to_le_bytes());
        doc.push(0x10); // int32
        doc.extend_from_slice(b"n\0");
        doc.extend_from_slice(&5i32.to_le_bytes());
        doc.push(0);
        let len = doc.len() as i32;
        doc[0..4].copy_from_slice(&len.to_le_bytes());
        let buf = build_op_msg(&doc);
        let result = parse_mongo_response(&buf).unwrap();
        assert!(result.contains("n=5"));
    }

    #[test]
    fn test_response_with_nmodified() {
        // {"ok": 1.0, "n": 3, "nModified": 2}
        let mut doc = Vec::new();
        doc.extend_from_slice(&[0; 4]);
        doc.push(0x01); doc.extend_from_slice(b"ok\0");
        doc.extend_from_slice(&1.0f64.to_le_bytes());
        doc.push(0x10); doc.extend_from_slice(b"n\0");
        doc.extend_from_slice(&3i32.to_le_bytes());
        doc.push(0x10); doc.extend_from_slice(b"nModified\0");
        doc.extend_from_slice(&2i32.to_le_bytes());
        doc.push(0);
        let len = doc.len() as i32;
        doc[0..4].copy_from_slice(&len.to_le_bytes());
        let buf = build_op_msg(&doc);
        let result = parse_mongo_response(&buf).unwrap();
        assert!(result.contains("modified=2"));
    }

    #[test]
    fn test_response_cursor_result() {
        // {"ok": 1.0, "cursor": {"firstBatch": [...]}}
        let mut batch_doc = Vec::new();
        batch_doc.extend_from_slice(&[0; 4]);
        batch_doc.push(0x10); batch_doc.extend_from_slice(b"0\0");
        batch_doc.extend_from_slice(&1i32.to_le_bytes());
        batch_doc.push(0);
        let batch_len = batch_doc.len() as i32;
        batch_doc[0..4].copy_from_slice(&batch_len.to_le_bytes());

        let mut cursor_doc = Vec::new();
        cursor_doc.extend_from_slice(&[0; 4]);
        cursor_doc.push(0x04); // array type
        cursor_doc.extend_from_slice(b"firstBatch\0");
        cursor_doc.extend_from_slice(&batch_doc);
        cursor_doc.push(0);
        let cursor_len = cursor_doc.len() as i32;
        cursor_doc[0..4].copy_from_slice(&cursor_len.to_le_bytes());

        let mut doc = Vec::new();
        doc.extend_from_slice(&[0; 4]);
        doc.push(0x01); doc.extend_from_slice(b"ok\0");
        doc.extend_from_slice(&1.0f64.to_le_bytes());
        doc.push(0x03); doc.extend_from_slice(b"cursor\0");
        doc.extend_from_slice(&cursor_doc);
        doc.push(0);
        let len = doc.len() as i32;
        doc[0..4].copy_from_slice(&len.to_le_bytes());

        let buf = build_op_msg(&doc);
        let result = parse_mongo_response(&buf).unwrap();
        assert!(result.contains("1 docs"));
    }

    #[test]
    fn test_extract_full_command_insert() {
        let doc = build_simple_cmd("insert", "products");
        let buf = build_op_msg(&doc);
        let result = extract_mongo_full_command(&buf).unwrap();
        assert!(result.contains("db.products.insertOne") || result.contains("db.products.insertMany"));
    }

    #[test]
    fn test_extract_full_command_update() {
        let doc = build_simple_cmd("update", "orders");
        let buf = build_op_msg(&doc);
        let result = extract_mongo_full_command(&buf).unwrap();
        assert!(result.contains("db.orders."));
    }

    #[test]
    fn test_extract_full_command_delete() {
        let doc = build_simple_cmd("delete", "logs");
        let buf = build_op_msg(&doc);
        let result = extract_mongo_full_command(&buf).unwrap();
        assert!(result.contains("db.logs."));
    }

    #[test]
    fn test_extract_full_command_aggregate() {
        let doc = build_simple_cmd("aggregate", "metrics");
        let buf = build_op_msg(&doc);
        let result = extract_mongo_full_command(&buf).unwrap();
        assert!(result.contains("db.metrics.aggregate"));
    }

    #[test]
    fn test_unknown_command_with_db() {
        // build_simple_cmd creates: {cmd: "ping", $db: "testdb"}
        let doc = build_simple_cmd("ping", "admin");
        let buf = build_op_msg(&doc);
        let result = parse_mongo_request(&buf).unwrap();
        assert!(result.contains("ping"));
        assert!(result.contains("testdb")); // $db field is always "testdb" in build_simple_cmd
    }
}
