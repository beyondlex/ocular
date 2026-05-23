/// MongoDB wire protocol parser (OP_MSG only, modern MongoDB 3.6+)
/// All integers are little-endian.

const OP_MSG: i32 = 2013;
const OP_COMPRESSED: i32 = 2012;

/// Get the total message length from a MongoDB wire protocol header.
/// Returns None if buffer is too small or length is invalid.
pub fn mongo_msg_len(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 { return None; }
    let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len < 16 || len > 48 * 1024 * 1024 { return None; }
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
    if opcode == OP_COMPRESSED { return None; } // skip compressed for now
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

/// Get a string field value from a BSON document.
fn get_string_field(doc: &[u8], name: &str) -> Option<String> {
    let mut pos = 4; // skip doc size
    while pos < doc.len() - 1 {
        let etype = doc[pos];
        if etype == 0 { break; } // end of doc
        pos += 1;
        let key = read_cstr(&doc[pos..])?;
        pos += key.len() + 1;
        match etype {
            0x02 => { // string
                if pos + 4 > doc.len() { return None; }
                let slen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize;
                pos += 4;
                if key == name {
                    let s = String::from_utf8_lossy(&doc[pos..pos+slen.saturating_sub(1)]).to_string();
                    return Some(s);
                }
                pos += slen;
            }
            0x01 => { pos += 8; } // double
            0x03 | 0x04 => { // document or array
                if pos + 4 > doc.len() { return None; }
                let dlen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize;
                pos += dlen;
            }
            0x05 => { // binary
                if pos + 4 > doc.len() { return None; }
                let blen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize;
                pos += 5 + blen;
            }
            0x07 => { pos += 12; } // ObjectId
            0x08 => { pos += 1; } // boolean
            0x09 | 0x11 | 0x12 => { pos += 8; } // datetime, timestamp, int64
            0x0A => {} // null
            0x10 => { pos += 4; } // int32
            0x13 => { pos += 16; } // decimal128
            _ => { return None; } // unknown type, bail
        }
    }
    None
}

fn get_f64_field(doc: &[u8], name: &str) -> Option<f64> {
    let mut pos = 4;
    while pos < doc.len() - 1 {
        let etype = doc[pos];
        if etype == 0 { break; }
        pos += 1;
        let key = read_cstr(&doc[pos..])?;
        pos += key.len() + 1;
        match etype {
            0x01 => {
                if key == name && pos + 8 <= doc.len() {
                    return Some(f64::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3], doc[pos+4], doc[pos+5], doc[pos+6], doc[pos+7]]));
                }
                pos += 8;
            }
            0x10 => {
                if key == name && pos + 4 <= doc.len() {
                    let v = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]);
                    return Some(v as f64);
                }
                pos += 4;
            }
            0x02 => { if pos + 4 > doc.len() { return None; } let slen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize; pos += 4 + slen; }
            0x03 | 0x04 => { if pos + 4 > doc.len() { return None; } let dlen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize; pos += dlen; }
            0x05 => { if pos + 4 > doc.len() { return None; } let blen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize; pos += 5 + blen; }
            0x07 => { pos += 12; }
            0x08 => { pos += 1; }
            0x09 | 0x11 | 0x12 => { pos += 8; }
            0x0A => {}
            0x13 => { pos += 16; }
            _ => { return None; }
        }
    }
    None
}

fn get_i32_field(doc: &[u8], name: &str) -> Option<i32> {
    let mut pos = 4;
    while pos < doc.len() - 1 {
        let etype = doc[pos];
        if etype == 0 { break; }
        pos += 1;
        let key = read_cstr(&doc[pos..])?;
        pos += key.len() + 1;
        match etype {
            0x10 => {
                if key == name && pos + 4 <= doc.len() {
                    return Some(i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]));
                }
                pos += 4;
            }
            0x01 => { pos += 8; }
            0x02 => { if pos + 4 > doc.len() { return None; } let slen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize; pos += 4 + slen; }
            0x03 | 0x04 => { if pos + 4 > doc.len() { return None; } let dlen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize; pos += dlen; }
            0x05 => { if pos + 4 > doc.len() { return None; } let blen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize; pos += 5 + blen; }
            0x07 => { pos += 12; }
            0x08 => { pos += 1; }
            0x09 | 0x11 | 0x12 => { pos += 8; }
            0x0A => {}
            0x13 => { pos += 16; }
            _ => { return None; }
        }
    }
    None
}

fn get_raw_doc_field<'a>(doc: &'a [u8], name: &str) -> Option<Vec<u8>> {
    let mut pos = 4;
    while pos < doc.len() - 1 {
        let etype = doc[pos];
        if etype == 0 { break; }
        pos += 1;
        let key = read_cstr(&doc[pos..])?;
        pos += key.len() + 1;
        match etype {
            0x03 | 0x04 => {
                if pos + 4 > doc.len() { return None; }
                let dlen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize;
                if key == name {
                    return Some(doc[pos..pos+dlen].to_vec());
                }
                pos += dlen;
            }
            0x01 => { pos += 8; }
            0x02 => { if pos + 4 > doc.len() { return None; } let slen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize; pos += 4 + slen; }
            0x05 => { if pos + 4 > doc.len() { return None; } let blen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize; pos += 5 + blen; }
            0x07 => { pos += 12; }
            0x08 => { pos += 1; }
            0x09 | 0x10 | 0x11 | 0x12 => { pos += if etype == 0x10 { 4 } else { 8 }; }
            0x0A => {}
            0x13 => { pos += 16; }
            _ => { return None; }
        }
    }
    None
}

fn has_field(doc: &[u8], name: &str) -> bool {
    let mut pos = 4;
    while pos < doc.len().saturating_sub(1) {
        let etype = doc[pos];
        if etype == 0 { break; }
        pos += 1;
        let Some(key) = read_cstr(&doc[pos..]) else { break };
        if key == name { return true; }
        pos += key.len() + 1;
        match etype {
            0x01 => { pos += 8; }
            0x02 => { if pos + 4 > doc.len() { break; } let slen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize; pos += 4 + slen; }
            0x03 | 0x04 => { if pos + 4 > doc.len() { break; } let dlen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize; pos += dlen; }
            0x05 => { if pos + 4 > doc.len() { break; } let blen = i32::from_le_bytes([doc[pos], doc[pos+1], doc[pos+2], doc[pos+3]]) as usize; pos += 5 + blen; }
            0x07 => { pos += 12; }
            0x08 => { pos += 1; }
            0x09 | 0x11 | 0x12 => { pos += 8; }
            0x0A => {}
            0x10 => { pos += 4; }
            0x13 => { pos += 16; }
            _ => { break; }
        }
    }
    false
}

fn get_array_len(doc: &[u8], name: &str) -> usize {
    let Some(arr) = get_raw_doc_field(doc, name) else { return 0 };
    // BSON array is a document with "0", "1", ... keys
    let mut count = 0;
    let mut pos = 4;
    while pos < arr.len().saturating_sub(1) {
        if arr[pos] == 0 { break; }
        count += 1;
        pos += 1;
        let Some(key) = read_cstr(&arr[pos..]) else { break };
        pos += key.len() + 1;
        match arr[pos - key.len() - 1..pos].first().copied().unwrap_or(0) { _ => {} }
        // skip value based on type
        let etype = arr[pos - key.len() - 2];
        match etype {
            0x01 => { pos += 8; }
            0x02 => { if pos + 4 > arr.len() { break; } let slen = i32::from_le_bytes([arr[pos], arr[pos+1], arr[pos+2], arr[pos+3]]) as usize; pos += 4 + slen; }
            0x03 | 0x04 => { if pos + 4 > arr.len() { break; } let dlen = i32::from_le_bytes([arr[pos], arr[pos+1], arr[pos+2], arr[pos+3]]) as usize; pos += dlen; }
            0x05 => { if pos + 4 > arr.len() { break; } let blen = i32::from_le_bytes([arr[pos], arr[pos+1], arr[pos+2], arr[pos+3]]) as usize; pos += 5 + blen; }
            0x07 => { pos += 12; }
            0x08 => { pos += 1; }
            0x09 | 0x11 | 0x12 => { pos += 8; }
            0x0A => {}
            0x10 => { pos += 4; }
            0x13 => { pos += 16; }
            _ => { break; }
        }
    }
    count
}

fn get_array_docs(doc: &[u8], name: &str) -> Vec<Vec<u8>> {
    let Some(arr) = get_raw_doc_field(doc, name) else { return vec![] };
    let mut docs = Vec::new();
    let mut pos = 4;
    while pos < arr.len().saturating_sub(1) {
        let etype = arr[pos];
        if etype == 0 { break; }
        pos += 1;
        let Some(key) = read_cstr(&arr[pos..]) else { break };
        pos += key.len() + 1;
        if etype == 0x03 {
            if pos + 4 > arr.len() { break; }
            let dlen = i32::from_le_bytes([arr[pos], arr[pos+1], arr[pos+2], arr[pos+3]]) as usize;
            if pos + dlen <= arr.len() {
                docs.push(arr[pos..pos+dlen].to_vec());
            }
            pos += dlen;
        } else {
            break; // unexpected type in result array
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
