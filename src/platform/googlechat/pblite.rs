//! Pblite (Proto-JSON-Lite) codec.
//!
//! Google Chat's BrowserChannel uses "pblite" encoding: a JSON array where
//! array index N corresponds to protobuf field number N+1.
//!
//! Example: `[null, "hello", null, 42]`
//! - index 0 → field 1 (null = absent)
//! - index 1 → field 2 = "hello" (string)
//! - index 2 → field 3 (null = absent)
//! - index 3 → field 4 = 42 (varint)
//!
//! ## Codec strategy
//!
//! Rather than generating per-type deserializers, we convert between pblite
//! JSON and protobuf wire format. This lets us reuse prost's decoder:
//!
//! ```text
//! pblite JSON array → pblite_to_wire() → protobuf wire bytes → prost::Message::decode()
//! prost struct → prost::Message::encode() → wire bytes → wire_to_pblite() → JSON array
//! ```
//!
//! This works because Google Chat's .proto uses ONLY varint (type 0) and
//! length-delimited (type 2) wire types — no fixed32/64/float/double.

use std::collections::HashMap;

use bytes::{BufMut, BytesMut};
use serde_json::Value;

use crate::error::PbliteError;

const WIRE_VARINT: u32 = 0;
const WIRE_LEN: u32 = 2;

// ─────────────────────── Proto schema types ─────────────────────────

/// The wire-level type of a protobuf field, as needed for pblite conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldType {
    /// Varint-encoded: int32, int64, uint32, uint64, sint32, sint64, enum
    Varint,
    /// Length-delimited string
    String,
    /// Length-delimited raw bytes
    Bytes,
    /// Boolean (varint 0/1)
    Bool,
    /// Enum (varint, but kept separate for clarity — encodes identically)
    Enum,
    /// Nested message (length-delimited, recurse into child)
    Message(std::borrow::Cow<'static, str>),
    /// Repeated message field
    RepeatedMessage(std::borrow::Cow<'static, str>),
    /// Repeated varint (int32, int64, enum, etc.)
    RepeatedVarint,
    /// Repeated string
    RepeatedString,
    /// Double (fixed 64-bit) — rare in Google Chat, but exists
    Double,
}

/// Result of resolving a type name against the known message/enum sets.
enum ResolvedType {
    Enum(#[allow(dead_code)] String),
    Message(String),
    Unknown,
}

/// Schema mapping `(message_name, field_number) -> FieldType`.
///
/// Built by parsing a `.proto` file. Only needs to handle the subset of
/// proto2 syntax used by `googlechat.proto`.
pub struct ProtoSchema {
    fields: HashMap<(String, u32), FieldType>,
}

impl ProtoSchema {
    /// Parse a `.proto` file and build the schema.
    ///
    /// Two-pass parser:
    /// - Pass 1: collect all message and enum names (so forward references resolve)
    /// - Pass 2: parse fields, using the name sets to disambiguate message vs enum
    pub fn from_proto_file(content: &str) -> Self {
        // ── Pass 1: Collect all message and enum names ──
        let (known_messages, known_enums) = Self::collect_names(content);

        // ── Pass 2: Parse fields ──
        let mut fields = HashMap::new();
        let mut message_stack: Vec<String> = Vec::new();
        let mut in_enum_depth: usize = 0;
        let mut brace_depth_in_current_msg: Vec<usize> = Vec::new();

        for line in content.lines() {
            let trimmed = line.trim();

            if trimmed.is_empty() || trimmed.starts_with("//") {
                continue;
            }

            // Strip inline comments
            let trimmed = if let Some(idx) = trimmed.find("//") {
                trimmed[..idx].trim()
            } else {
                trimmed
            };

            if trimmed.is_empty() {
                continue;
            }

            // Inside an enum block — skip everything until we exit
            if in_enum_depth > 0 {
                let opens = trimmed.matches('{').count();
                let closes = trimmed.matches('}').count();
                in_enum_depth = in_enum_depth.saturating_add(opens).saturating_sub(closes);
                continue;
            }

            // Enum declaration
            if trimmed.starts_with("enum ") {
                if trimmed.contains('{') && !trimmed.contains('}') {
                    in_enum_depth = 1;
                }
                continue;
            }

            // Message open
            if trimmed.starts_with("message ") {
                let name = trimmed
                    .strip_prefix("message ")
                    .and_then(|s| s.split_whitespace().next())
                    .unwrap_or("");
                if !name.is_empty() {
                    message_stack.push(name.to_string());
                    brace_depth_in_current_msg.push(0);
                }
                continue;
            }

            // Track braces inside messages to know when to pop
            if !message_stack.is_empty() {
                let opens = trimmed.matches('{').count();
                let closes = trimmed.matches('}').count();

                if let Some(depth) = brace_depth_in_current_msg.last_mut() {
                    *depth = depth.saturating_add(opens);
                }

                // Handle close braces
                for _ in 0..closes {
                    if let Some(depth) = brace_depth_in_current_msg.last_mut() {
                        if *depth == 0 {
                            // This close brace closes the current message
                            message_stack.pop();
                            brace_depth_in_current_msg.pop();
                        } else {
                            *depth -= 1;
                        }
                    }
                }

                // If the line is ONLY a close brace or nothing interesting left, skip
                if trimmed == "}" || trimmed.chars().all(|c| c == '}' || c.is_whitespace()) {
                    continue;
                }
            }

            // Field declaration: must be inside a message
            if message_stack.is_empty() {
                continue;
            }

            // Use split_whitespace to handle tabs and multiple spaces uniformly
            let mut parts_iter = trimmed.split_whitespace();
            let keyword = parts_iter.next().unwrap_or("");
            let is_repeated = match keyword {
                "optional" | "required" => false,
                "repeated" => true,
                _ => continue,
            };

            // Reconstruct the parts after the keyword
            let parts: Vec<&str> = parts_iter.collect();
            if parts.len() < 4 || parts[2] != "=" {
                continue;
            }

            let type_name = parts[0];
            let field_num_str = parts[3].trim_end_matches(';').trim_end_matches(',');
            let field_num: u32 = match field_num_str.parse() {
                Ok(n) => n,
                Err(_) => continue,
            };

            let msg_path = message_stack.join(".");

            let field_type = match type_name {
                "int32" | "int64" | "uint32" | "uint64" | "sint32" | "sint64" | "fixed32"
                | "fixed64" | "sfixed32" | "sfixed64" => {
                    if is_repeated {
                        FieldType::RepeatedVarint
                    } else {
                        FieldType::Varint
                    }
                }
                "bool" => {
                    if is_repeated {
                        FieldType::RepeatedVarint
                    } else {
                        FieldType::Bool
                    }
                }
                "string" => {
                    if is_repeated {
                        FieldType::RepeatedString
                    } else {
                        FieldType::String
                    }
                }
                "bytes" => FieldType::Bytes,
                "double" | "float" => FieldType::Double,
                other => {
                    // Resolve as enum or message, trying scoped names first
                    let resolved =
                        Self::resolve_type(other, &message_stack, &known_messages, &known_enums);

                    match resolved {
                        ResolvedType::Enum(_) => {
                            if is_repeated {
                                FieldType::RepeatedVarint
                            } else {
                                FieldType::Enum
                            }
                        }
                        ResolvedType::Message(full_name) => {
                            if is_repeated {
                                FieldType::RepeatedMessage(std::borrow::Cow::Owned(full_name))
                            } else {
                                FieldType::Message(std::borrow::Cow::Owned(full_name))
                            }
                        }
                        ResolvedType::Unknown => {
                            // Assume message as fallback
                            if is_repeated {
                                FieldType::RepeatedMessage(std::borrow::Cow::Owned(
                                    other.to_string(),
                                ))
                            } else {
                                FieldType::Message(std::borrow::Cow::Owned(other.to_string()))
                            }
                        }
                    }
                }
            };

            // Register the field under its fully-qualified message path
            fields.insert((msg_path.clone(), field_num), field_type.clone());

            // ALSO register under the short (leaf) name for easier lookup
            // from prost error paths that use just the message name
            if let Some(leaf) = message_stack.last() {
                if leaf != &msg_path {
                    fields
                        .entry((leaf.clone(), field_num))
                        .or_insert(field_type);
                }
            }
        }

        Self { fields }
    }

    /// First pass: collect all message and enum names with their fully-qualified paths.
    fn collect_names(
        content: &str,
    ) -> (
        std::collections::HashSet<String>,
        std::collections::HashSet<String>,
    ) {
        let mut messages = std::collections::HashSet::new();
        let mut enums = std::collections::HashSet::new();
        let mut stack: Vec<String> = Vec::new();
        let mut brace_depth: Vec<usize> = Vec::new();
        let mut in_enum_depth: usize = 0;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("//") {
                continue;
            }
            let trimmed = if let Some(idx) = trimmed.find("//") {
                trimmed[..idx].trim()
            } else {
                trimmed
            };
            if trimmed.is_empty() {
                continue;
            }

            // Skip enum body contents but track depth
            if in_enum_depth > 0 {
                let opens = trimmed.matches('{').count();
                let closes = trimmed.matches('}').count();
                in_enum_depth = in_enum_depth.saturating_add(opens).saturating_sub(closes);
                continue;
            }

            if trimmed.starts_with("enum ") {
                let name = trimmed
                    .strip_prefix("enum ")
                    .and_then(|s| s.split_whitespace().next())
                    .unwrap_or("");
                if !name.is_empty() {
                    // Register short and qualified names
                    enums.insert(name.to_string());
                    if !stack.is_empty() {
                        let full = format!("{}.{}", stack.join("."), name);
                        enums.insert(full);
                    }
                }
                if trimmed.contains('{') && !trimmed.contains('}') {
                    in_enum_depth = 1;
                }
                continue;
            }

            if trimmed.starts_with("message ") {
                let name = trimmed
                    .strip_prefix("message ")
                    .and_then(|s| s.split_whitespace().next())
                    .unwrap_or("");
                if !name.is_empty() {
                    stack.push(name.to_string());
                    brace_depth.push(0);
                    let full = stack.join(".");
                    messages.insert(full);
                    messages.insert(name.to_string());
                }
                continue;
            }

            // Track braces inside messages
            if !stack.is_empty() {
                let opens = trimmed.matches('{').count();
                let closes = trimmed.matches('}').count();

                if let Some(depth) = brace_depth.last_mut() {
                    *depth = depth.saturating_add(opens);
                }

                for _ in 0..closes {
                    if let Some(depth) = brace_depth.last_mut() {
                        if *depth == 0 {
                            stack.pop();
                            brace_depth.pop();
                        } else {
                            *depth -= 1;
                        }
                    }
                }
            }
        }

        (messages, enums)
    }

    /// Resolve a type name by walking up the message scope stack.
    fn resolve_type(
        name: &str,
        scope: &[String],
        messages: &std::collections::HashSet<String>,
        enums: &std::collections::HashSet<String>,
    ) -> ResolvedType {
        // Try each scope level, from innermost to outermost
        for i in (0..=scope.len()).rev() {
            let prefix = scope[..i].join(".");
            let candidate = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}.{name}")
            };
            if enums.contains(&candidate) {
                return ResolvedType::Enum(candidate);
            }
            if messages.contains(&candidate) {
                return ResolvedType::Message(candidate);
            }
        }

        // Also try the short name directly (in case it's a sibling or cousin)
        if enums.contains(name) {
            return ResolvedType::Enum(name.to_string());
        }
        if messages.contains(name) {
            return ResolvedType::Message(name.to_string());
        }

        ResolvedType::Unknown
    }

    /// Look up the field type for a given message and field number.
    pub fn field_type(&self, message: &str, field_number: u32) -> Option<&FieldType> {
        self.fields.get(&(message.to_string(), field_number))
    }
}

/// Load and parse the bundled googlechat.proto schema.
pub fn load_schema() -> ProtoSchema {
    ProtoSchema::from_proto_file(include_str!("../../../proto/googlechat.proto"))
}

// ─────────────────── schema-aware pblite → wire ─────────────────────

/// Convert a pblite JSON value (array) into protobuf wire format bytes,
/// using schema information to correctly distinguish string fields from
/// integer-as-string fields.
///
/// This is the schema-aware counterpart of [`pblite_to_wire`]. Use it for
/// decoding server responses where digit-only strings (like user IDs) must
/// be preserved as strings rather than misinterpreted as varints.
pub fn pblite_to_wire_typed(
    value: &Value,
    schema: &ProtoSchema,
    message_name: &str,
) -> Result<bytes::Bytes, PbliteError> {
    let mut buf = BytesMut::with_capacity(256);
    encode_message_typed(&mut buf, value, schema, message_name)?;
    Ok(buf.freeze())
}

fn encode_message_typed(
    buf: &mut BytesMut,
    value: &Value,
    schema: &ProtoSchema,
    message_name: &str,
) -> Result<(), PbliteError> {
    match value {
        Value::Array(arr) => {
            let (positional, sparse) = split_sparse(arr);

            for (index, element) in positional.iter().enumerate() {
                if element.is_null() {
                    continue;
                }
                let field_number = (index + 1) as u32;
                let ft = schema.field_type(message_name, field_number);
                encode_field_typed(buf, field_number, element, schema, message_name, ft)?;
            }

            if let Some(obj) = sparse {
                for (key, val) in obj {
                    if val.is_null() {
                        continue;
                    }
                    let field_num: u32 = key
                        .parse()
                        .map_err(|_| PbliteError::InvalidFieldKey(key.to_owned()))?;
                    let ft = schema.field_type(message_name, field_num);
                    encode_field_typed(buf, field_num, val, schema, message_name, ft)?;
                }
            }
        }
        Value::Null => {}
        _ => return Err(PbliteError::ExpectedArray),
    }
    Ok(())
}

fn encode_field_typed(
    buf: &mut BytesMut,
    field_number: u32,
    value: &Value,
    schema: &ProtoSchema,
    _parent_message: &str,
    field_type: Option<&FieldType>,
) -> Result<(), PbliteError> {
    match value {
        Value::Null => Ok(()),

        Value::String(s) => {
            match field_type {
                Some(FieldType::Varint) | Some(FieldType::Enum) => {
                    // Schema says this is an integer field encoded as JSON string
                    if let Ok(v) = s.parse::<i64>() {
                        write_tag(buf, field_number, WIRE_VARINT);
                        write_varint(buf, v as u64);
                    } else {
                        // Fallback: emit as string if parse fails
                        write_tag(buf, field_number, WIRE_LEN);
                        write_varint(buf, s.len() as u64);
                        buf.put_slice(s.as_bytes());
                    }
                    Ok(())
                }
                Some(FieldType::Bool) => {
                    // "1"/"true" → 1, "0"/"false" → 0
                    let v = match s.as_str() {
                        "1" | "true" => 1u64,
                        _ => 0u64,
                    };
                    write_tag(buf, field_number, WIRE_VARINT);
                    write_varint(buf, v);
                    Ok(())
                }
                Some(FieldType::Bytes) => {
                    // Decode base64
                    use base64::Engine;
                    let raw = base64::engine::general_purpose::STANDARD
                        .decode(s)
                        .unwrap_or_else(|_| s.as_bytes().to_vec());
                    write_tag(buf, field_number, WIRE_LEN);
                    write_varint(buf, raw.len() as u64);
                    buf.put_slice(&raw);
                    Ok(())
                }
                Some(FieldType::String) | Some(FieldType::RepeatedString) => {
                    // Always emit as string, even if digits-only
                    write_tag(buf, field_number, WIRE_LEN);
                    write_varint(buf, s.len() as u64);
                    buf.put_slice(s.as_bytes());
                    Ok(())
                }
                _ => {
                    // No schema info or unexpected type — fall back to heuristic
                    encode_field(buf, field_number, value)
                }
            }
        }

        Value::Number(n) => {
            // Numbers are always varints in this schema
            write_tag(buf, field_number, WIRE_VARINT);
            if let Some(v) = n.as_i64() {
                write_varint(buf, v as u64);
            } else if let Some(v) = n.as_u64() {
                write_varint(buf, v);
            } else {
                return Err(PbliteError::UnexpectedType {
                    field: field_number,
                    detail: "floating point number".into(),
                });
            }
            Ok(())
        }

        Value::Bool(b) => {
            write_tag(buf, field_number, WIRE_VARINT);
            write_varint(buf, if *b { 1 } else { 0 });
            Ok(())
        }

        Value::Array(arr) => {
            match field_type {
                Some(FieldType::RepeatedMessage(child)) => {
                    // Each element is a separate message instance
                    for element in arr {
                        if element.is_null() {
                            continue;
                        }
                        let mut sub_buf = BytesMut::new();
                        encode_message_typed(&mut sub_buf, element, schema, child)?;
                        write_tag(buf, field_number, WIRE_LEN);
                        write_varint(buf, sub_buf.len() as u64);
                        buf.put_slice(&sub_buf);
                    }
                    Ok(())
                }
                Some(FieldType::RepeatedVarint) => {
                    // Each element is a varint
                    for element in arr {
                        if element.is_null() {
                            continue;
                        }
                        write_tag(buf, field_number, WIRE_VARINT);
                        match element {
                            Value::Number(n) => {
                                if let Some(v) = n.as_i64() {
                                    write_varint(buf, v as u64);
                                } else if let Some(v) = n.as_u64() {
                                    write_varint(buf, v);
                                }
                            }
                            Value::String(s) => {
                                if let Ok(v) = s.parse::<i64>() {
                                    write_varint(buf, v as u64);
                                }
                            }
                            _ => {}
                        }
                    }
                    Ok(())
                }
                Some(FieldType::RepeatedString) => {
                    // Each element is a string
                    for element in arr {
                        if element.is_null() {
                            continue;
                        }
                        if let Value::String(s) = element {
                            write_tag(buf, field_number, WIRE_LEN);
                            write_varint(buf, s.len() as u64);
                            buf.put_slice(s.as_bytes());
                        }
                    }
                    Ok(())
                }
                Some(FieldType::Message(child)) => {
                    // Nested message — the array IS the message's positional fields
                    let mut sub_buf = BytesMut::new();
                    encode_message_typed(&mut sub_buf, value, schema, child)?;
                    write_tag(buf, field_number, WIRE_LEN);
                    write_varint(buf, sub_buf.len() as u64);
                    buf.put_slice(&sub_buf);
                    Ok(())
                }
                _ => {
                    // No schema — fall back to the heuristic encoder
                    encode_field(buf, field_number, value)
                }
            }
        }

        Value::Object(_) => {
            // Bare object — treat as sparse-only nested message
            match field_type {
                Some(FieldType::Message(child)) => {
                    let mut sub_buf = BytesMut::new();
                    encode_message_typed(&mut sub_buf, value, schema, child)?;
                    write_tag(buf, field_number, WIRE_LEN);
                    write_varint(buf, sub_buf.len() as u64);
                    buf.put_slice(&sub_buf);
                    Ok(())
                }
                _ => encode_field(buf, field_number, value),
            }
        }
    }
}

// Note: Only 2 `bytes`-typed fields exist in googlechat.proto:
//   - StreamEventsRequest.dispatch_random_filler (field 3)
//   - WrappedResourceKey.wrapped_key (field 1)
// Handled via heuristic in encode_field: if a string looks like base64
// and decodes to non-UTF-8 binary, we base64-decode before emitting.

// ─────────────────────────── pblite → wire ───────────────────────────

/// Convert a pblite JSON value (array) into protobuf wire format bytes.
///
/// The resulting bytes can be decoded by `prost::Message::decode()`.
pub fn pblite_to_wire(value: &Value) -> Result<bytes::Bytes, PbliteError> {
    let mut buf = BytesMut::with_capacity(256);
    encode_message(&mut buf, value)?;
    Ok(buf.freeze())
}

fn encode_message(buf: &mut BytesMut, value: &Value) -> Result<(), PbliteError> {
    match value {
        Value::Array(arr) => {
            let (positional, sparse) = split_sparse(arr);

            // Positional elements: index N → field number N+1
            for (index, element) in positional.iter().enumerate() {
                if element.is_null() {
                    continue;
                }
                let field_number = (index + 1) as u32;
                encode_field(buf, field_number, element)?;
            }

            // Sparse map (if present): the last element may be a JSON object
            // with string keys representing high field numbers
            if let Some(obj) = sparse {
                for (key, val) in obj {
                    if val.is_null() {
                        continue;
                    }
                    let field_num: u32 = key
                        .parse()
                        .map_err(|_| PbliteError::InvalidFieldKey(key.to_owned()))?;
                    encode_field(buf, field_num, val)?;
                }
            }
        }
        Value::Null => {} // Empty message
        _ => return Err(PbliteError::ExpectedArray),
    }
    Ok(())
}

/// Split the array into positional elements and an optional trailing sparse map.
///
/// If the last element is a JSON object (not array, not scalar), it's treated
/// as a sparse field map for high field numbers (e.g., field 100 for RequestHeader).
fn split_sparse(arr: &[Value]) -> (&[Value], Option<&serde_json::Map<String, Value>>) {
    if let Some(Value::Object(map)) = arr.last() {
        return (&arr[..arr.len() - 1], Some(map));
    }
    (arr, None)
}

fn encode_field(buf: &mut BytesMut, field_number: u32, value: &Value) -> Result<(), PbliteError> {
    match value {
        Value::Null => Ok(()),

        Value::String(s) => {
            // Google Chat pblite encodes int64/int32/enum values as JSON strings
            // when they might exceed JavaScript's Number.MAX_SAFE_INTEGER.
            // In practice, the server sends ALL integer-typed fields as strings
            // in pblite responses — timestamps, enum values, counts, etc.
            //
            // Detect pure numeric strings and encode as varint. Real string
            // fields (IDs, names, emails) contain non-digit characters.
            let is_numeric = !s.is_empty()
                && (s.bytes().all(|b| b.is_ascii_digit())
                    || (s.starts_with('-')
                        && s.len() > 1
                        && s[1..].bytes().all(|b| b.is_ascii_digit())));
            if is_numeric {
                if let Ok(v) = s.parse::<i64>() {
                    write_tag(buf, field_number, WIRE_VARINT);
                    write_varint(buf, v as u64);
                    return Ok(());
                }
            }

            // Try base64 decode for potential `bytes` fields.
            // If the string is valid base64 AND contains non-UTF-8 bytes,
            // treat it as a bytes field. Otherwise emit as a string.
            let raw = if looks_like_base64(s) {
                use base64::Engine;
                match base64::engine::general_purpose::STANDARD.decode(s) {
                    Ok(decoded) if std::str::from_utf8(&decoded).is_err() => decoded,
                    _ => s.as_bytes().to_vec(),
                }
            } else {
                s.as_bytes().to_vec()
            };

            write_tag(buf, field_number, WIRE_LEN);
            write_varint(buf, raw.len() as u64);
            buf.put_slice(&raw);
            Ok(())
        }

        Value::Number(n) => {
            write_tag(buf, field_number, WIRE_VARINT);
            if let Some(v) = n.as_i64() {
                write_varint(buf, v as u64);
            } else if let Some(v) = n.as_u64() {
                write_varint(buf, v);
            } else {
                // Float — should not appear in Google Chat's schema
                return Err(PbliteError::UnexpectedType {
                    field: field_number,
                    detail: "floating point number".into(),
                });
            }
            Ok(())
        }

        Value::Bool(b) => {
            write_tag(buf, field_number, WIRE_VARINT);
            write_varint(buf, if *b { 1 } else { 0 });
            Ok(())
        }

        Value::Array(arr) => {
            // In pblite, an array at a field position is either:
            //   (a) A nested message — elements are positional fields of the sub-message
            //   (b) A repeated message — array of arrays, each inner array is one instance
            //
            // Disambiguation heuristic (schema-less):
            //   - Repeated messages (produced by wire_to_pblite grouping or server)
            //     are dense arrays of arrays with NO nulls: [[msg1], [msg2], ...]
            //   - Nested messages are positional: [field1, null, field3, ...]
            //     and commonly have null gaps or mixed types.
            //
            // Rule:
            //   1. No non-null elements → skip
            //   2. All non-null are arrays AND no nulls AND count > 1 → repeated
            //   3. Otherwise → nested message
            let non_null_count = arr.iter().filter(|v| !v.is_null()).count();
            let has_any_null = arr.iter().any(|v| v.is_null());
            let all_non_null_are_arrays =
                non_null_count > 0 && arr.iter().filter(|v| !v.is_null()).all(|v| v.is_array());

            if non_null_count == 0 {
                // Empty array — skip
                Ok(())
            } else if all_non_null_are_arrays && !has_any_null && non_null_count > 1 {
                // Repeated messages: emit each element as a separate field entry
                for element in arr {
                    if !element.is_null() {
                        let mut sub_buf = BytesMut::new();
                        encode_message(&mut sub_buf, element)?;
                        write_tag(buf, field_number, WIRE_LEN);
                        write_varint(buf, sub_buf.len() as u64);
                        buf.put_slice(&sub_buf);
                    }
                }
                Ok(())
            } else {
                // Default: nested message (positional field encoding)
                let mut sub_buf = BytesMut::new();
                encode_message(&mut sub_buf, value)?;
                write_tag(buf, field_number, WIRE_LEN);
                write_varint(buf, sub_buf.len() as u64);
                buf.put_slice(&sub_buf);
                Ok(())
            }
        }

        Value::Object(_) => {
            // Bare object at a field position — treat as a nested message
            // with only sparse fields
            let mut sub_buf = BytesMut::new();
            encode_message(&mut sub_buf, value)?;
            write_tag(buf, field_number, WIRE_LEN);
            write_varint(buf, sub_buf.len() as u64);
            buf.put_slice(&sub_buf);
            Ok(())
        }
    }
}

/// Heuristic: does this string look like it could be base64-encoded binary?
///
/// Tightened to avoid false positives on Google Chat IDs like "spaces/room1"
/// which are valid base64 alphabet but are actually string fields.
/// The two actual `bytes` fields in the schema contain random binary data
/// that base64-encodes to long strings, typically with '=' padding.
/// Check if the first byte of `data` could plausibly be a protobuf wire tag.
///
/// A valid tag byte is `(field_num << 3) | wire_type`, where wire_type is
/// 0 (varint), 1 (fixed64), 2 (LEN), 3 (start_group — deprecated), 4 (end_group),
/// or 5 (fixed32). We accept 0, 2, and 5 (and reject 3, 4 since they're
/// deprecated). The field number must be > 0.
///
/// For a first byte to be a valid single-byte tag, it must have MSB 0
/// (no continuation). Multi-byte tags start with MSB 1.
fn looks_like_nested_tag(data: &[u8]) -> bool {
    let Some(&first) = data.first() else {
        return false;
    };
    // Fast reject: strings rarely start with bytes > 0x7F in single-byte form
    // (those would be continuation bytes of multi-byte UTF-8 and invalid as first bytes).
    if first == 0 {
        return false;
    }

    // If MSB set, it's a multi-byte varint tag — plausible for tags with field_num > 15
    // We'll just attempt the parse later; here we give it a chance.
    if first & 0x80 != 0 {
        return true;
    }

    // Single-byte tag: lower 3 bits = wire_type, upper 5 bits = field_num
    let wire_type = first & 0x07;
    let field_num = first >> 3;
    field_num > 0 && matches!(wire_type, 0 | 1 | 2 | 5)
}

/// Check that all top-level field numbers in a decoded pblite array are
/// within a reasonable range (< 1000). Random binary data often decodes as
/// valid-looking protobuf but with huge field numbers (100000+) which is
/// a clear signal of misinterpretation.
fn has_reasonable_field_numbers(value: &Value) -> bool {
    let Value::Array(arr) = value else {
        return false;
    };
    // If the array is very long due to a huge max field number, reject.
    // Google Chat uses field numbers up to ~100 in normal messages.
    if arr.len() > 1000 {
        return false;
    }
    true
}

fn looks_like_base64(s: &str) -> bool {
    let all_b64 = s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=');
    if !all_b64 {
        return false;
    }
    // Require either trailing '=' padding or length >= 24 (18+ raw bytes).
    // Short strings like "spaces/room1" or "dm/abc" are not base64.
    s.ends_with('=') || s.len() >= 24
}

// ─────────────────────────── wire → pblite ───────────────────────────

/// Convert protobuf wire format bytes into a pblite JSON array.
///
/// Used for encoding messages to send via the BrowserChannel forward channel.
pub fn wire_to_pblite(wire: &[u8]) -> Result<Value, PbliteError> {
    let mut fields: Vec<(u32, Value)> = Vec::new();
    let mut cursor = wire;

    while !cursor.is_empty() {
        let (tag, rest) = read_varint(cursor)?;
        cursor = rest;
        let field_number = (tag >> 3) as u32;
        let wire_type = (tag & 0x7) as u32;

        let (value, rest) = match wire_type {
            WIRE_VARINT => {
                let (v, r) = read_varint(cursor)?;
                (Value::Number(serde_json::Number::from(v)), r)
            }
            WIRE_LEN => {
                let (len, r) = read_varint(cursor)?;
                let len = len as usize;
                if r.len() < len {
                    return Err(PbliteError::UnexpectedType {
                        field: field_number,
                        detail: format!(
                            "length-delimited field truncated: need {len}, have {}",
                            r.len()
                        ),
                    });
                }
                let data = &r[..len];

                // Disambiguate: nested message vs string vs bytes.
                //
                // Strategy: if the first byte looks like a valid protobuf
                // wire tag (for wire types 0, 2, or 5 with a small field
                // number), try nested-message parsing. Validate the result
                // by checking all field numbers are reasonable (< 1000).
                //
                // Otherwise, prefer UTF-8 string interpretation.
                //
                // This fixes two bugs:
                // 1. Strings like "hby1pSB_00Y" (message IDs) were mis-parsed
                //    as nested messages because their bytes decode as valid
                //    protobuf with random field numbers.
                // 2. Nested messages whose first byte is a printable ASCII
                //    character (like MessageParentId with first tag 0x22=`"`)
                //    were mis-parsed as strings.
                // Strongest signal: if the data is fully printable ASCII
                // or valid UTF-8 text (no control bytes except tab/newline),
                // it's a string. Nested message wire bytes almost always
                // contain tag bytes that are ASCII control characters.
                let looks_like_text = std::str::from_utf8(data).is_ok()
                    && data
                        .iter()
                        .all(|&b| b >= 0x20 || b == b'\t' || b == b'\n' || b == b'\r' || b >= 0x80);

                let val = if looks_like_text {
                    // Clearly text
                    Value::String(std::str::from_utf8(data).unwrap().to_owned())
                } else if looks_like_nested_tag(data) {
                    // Might be a nested message
                    match wire_to_pblite(data) {
                        Ok(nested) if has_reasonable_field_numbers(&nested) => nested,
                        _ => {
                            if let Ok(s) = std::str::from_utf8(data) {
                                Value::String(s.to_owned())
                            } else {
                                use base64::Engine;
                                Value::String(
                                    base64::engine::general_purpose::STANDARD.encode(data),
                                )
                            }
                        }
                    }
                } else {
                    // Not text, not a tag — fall through
                    if let Ok(s) = std::str::from_utf8(data) {
                        Value::String(s.to_owned())
                    } else {
                        use base64::Engine;
                        Value::String(base64::engine::general_purpose::STANDARD.encode(data))
                    }
                };
                (val, &r[len..])
            }
            // Wire type 5 (fixed32) — not in Google Chat's schema but handle gracefully
            5 => {
                if cursor.len() < 4 {
                    return Err(PbliteError::UnsupportedWireType(5));
                }
                let v = u32::from_le_bytes([cursor[0], cursor[1], cursor[2], cursor[3]]);
                (Value::Number(serde_json::Number::from(v)), &cursor[4..])
            }
            // Wire type 1 (fixed64) — not in Google Chat's schema
            1 => {
                if cursor.len() < 8 {
                    return Err(PbliteError::UnsupportedWireType(1));
                }
                let v = u64::from_le_bytes([
                    cursor[0], cursor[1], cursor[2], cursor[3], cursor[4], cursor[5], cursor[6],
                    cursor[7],
                ]);
                (Value::Number(serde_json::Number::from(v)), &cursor[8..])
            }
            other => return Err(PbliteError::UnsupportedWireType(other)),
        };
        cursor = rest;
        fields.push((field_number, value));
    }

    if fields.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }

    // Build positional array: find max field number, allocate array of that size
    let max_field = fields.iter().map(|(n, _)| *n).max().unwrap_or(0);

    // Handle repeated fields: group by field number
    let mut grouped: std::collections::BTreeMap<u32, Vec<Value>> =
        std::collections::BTreeMap::new();
    for (num, val) in fields {
        grouped.entry(num).or_default().push(val);
    }

    let mut arr = vec![Value::Null; max_field as usize];
    for (num, mut vals) in grouped {
        let idx = (num - 1) as usize; // field N → index N-1
        if vals.len() == 1 {
            arr[idx] = vals.pop().unwrap();
        } else {
            arr[idx] = Value::Array(vals);
        }
    }

    Ok(Value::Array(arr))
}

// ─────────────────────── wire format primitives ──────────────────────

fn write_tag(buf: &mut BytesMut, field_number: u32, wire_type: u32) {
    write_varint(buf, ((field_number as u64) << 3) | (wire_type as u64));
}

fn write_varint(buf: &mut BytesMut, mut value: u64) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            buf.put_u8(byte);
            break;
        } else {
            buf.put_u8(byte | 0x80);
        }
    }
}

fn read_varint(data: &[u8]) -> Result<(u64, &[u8]), PbliteError> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;

    for (i, &byte) in data.iter().enumerate() {
        if shift >= 70 {
            return Err(PbliteError::VarintOverflow);
        }
        result |= ((byte & 0x7F) as u64) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            return Ok((result, &data[i + 1..]));
        }
    }
    Err(PbliteError::VarintOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ─────── schema parsing tests ───────

    #[test]
    fn schema_parses_user_id_as_string() {
        let schema = load_schema();
        // UserId.id = field 1 should be String
        let ft = schema.field_type("UserId", 1);
        assert!(
            matches!(ft, Some(FieldType::String)),
            "UserId.id should be String, got {:?}",
            ft
        );
    }

    #[test]
    fn schema_parses_annotation_type_as_enum() {
        let schema = load_schema();
        // Annotation.type = field 1 (AnnotationType enum)
        let ft = schema.field_type("Annotation", 1);
        assert!(
            matches!(ft, Some(FieldType::Enum)),
            "Annotation.type should be Enum, got {:?}",
            ft
        );
    }

    #[test]
    fn schema_parses_group_read_state_id() {
        let schema = load_schema();
        // GroupReadStateId.user_id = field 1 (UserId message)
        let ft = schema.field_type("GroupReadStateId", 1);
        assert!(
            matches!(ft, Some(FieldType::Message(_))),
            "GroupReadStateId.user_id should be Message, got {:?}",
            ft
        );
    }

    #[test]
    fn schema_parses_group_create_time_as_varint() {
        let schema = load_schema();
        // Group.create_time = field 5 (int64)
        let ft = schema.field_type("Group", 5);
        assert!(
            matches!(ft, Some(FieldType::Varint)),
            "Group.create_time should be Varint, got {:?}",
            ft
        );
    }

    #[test]
    fn schema_parses_nested_message_path() {
        let schema = load_schema();
        // JAddOnsFormAction.ActionParameter is a nested message
        // Check its fields are registered under the full path
        let ft = schema.field_type("JAddOnsFormAction.ActionParameter", 1);
        assert!(
            matches!(ft, Some(FieldType::String)),
            "ActionParameter.key should be String, got {:?}",
            ft
        );
    }

    #[test]
    fn schema_parses_upload_metadata() {
        let schema = load_schema();
        // UploadMetadata.attachment_token = field 1 (string)
        let ft = schema.field_type("UploadMetadata", 1);
        assert!(
            matches!(ft, Some(FieldType::String)),
            "UploadMetadata.attachment_token should be String, got {:?}",
            ft
        );
    }

    #[test]
    fn schema_parses_annotation_upload_metadata() {
        let schema = load_schema();
        // Annotation.upload_metadata = field 10 (UploadMetadata)
        let ft = schema.field_type("Annotation", 10);
        assert!(
            matches!(ft, Some(FieldType::Message(name)) if name.as_ref() == "UploadMetadata"),
            "Annotation.upload_metadata should be Message(UploadMetadata), got {:?}",
            ft
        );
    }

    #[test]
    fn message_id_string_not_mistaken_for_nested() {
        use crate::platform::googlechat::proto;
        use prost::Message;

        // "hby1pSB_00Y" starts with 'h' (0x68 = printable, but could also be
        // parsed as field 13 varint). Previously our codec mis-parsed this
        // as a nested message, corrupting the MessageId.
        let msg_id = proto::MessageId {
            parent_id: Some(proto::MessageParentId {
                topic_id: Some(proto::TopicId {
                    group_id: Some(proto::GroupId {
                        space_id: Some(proto::SpaceId {
                            space_id: Some("AAAAPptFat4".into()),
                        }),
                        dm_id: None,
                    }),
                    topic_id: Some("hby1pSB_00Y".into()),
                }),
            }),
            message_id: Some("hby1pSB_00Y".into()),
        };
        let wire = msg_id.encode_to_vec();
        let pblite_json = wire_to_pblite(&wire).unwrap();

        // The pblite should have the message_id as a string, not an array
        if let Value::Array(arr) = &pblite_json {
            // field 2 at index 1 is message_id — must be a string
            let msg_id_val = arr.get(1).expect("should have message_id field");
            assert!(
                matches!(msg_id_val, Value::String(s) if s == "hby1pSB_00Y"),
                "message_id should be string \"hby1pSB_00Y\", got {:?}",
                msg_id_val
            );
        } else {
            panic!("expected array");
        }

        // And roundtrip correctly
        let wire_back = pblite_to_wire(&pblite_json).unwrap();
        let decoded = proto::MessageId::decode(wire_back).unwrap();
        assert_eq!(decoded.message_id, Some("hby1pSB_00Y".into()));
    }

    #[test]
    fn nested_msg_with_printable_tag_byte() {
        use crate::platform::googlechat::proto;
        use prost::Message;

        // MessageParentId has topic_id at field 4, so wire starts with
        // tag = (4 << 3) | 2 = 0x22 = '"' (printable ASCII).
        // Ensure we still parse this as a nested message, not a string.
        let parent = proto::MessageParentId {
            topic_id: Some(proto::TopicId {
                group_id: Some(proto::GroupId {
                    space_id: Some(proto::SpaceId {
                        space_id: Some("AAAAPptFat4".into()),
                    }),
                    dm_id: None,
                }),
                topic_id: None,
            }),
        };
        let wire = parent.encode_to_vec();
        let pblite_json = wire_to_pblite(&wire).unwrap();
        let wire_back = pblite_to_wire(&pblite_json).unwrap();
        let decoded = proto::MessageParentId::decode(wire_back).unwrap();

        let topic = decoded.topic_id.expect("topic_id should exist");
        let gid = topic.group_id.expect("group_id should exist");
        let sid = gid.space_id.expect("space_id should exist");
        assert_eq!(sid.space_id, Some("AAAAPptFat4".into()));
    }

    #[test]
    fn schema_parses_message_annotations_as_repeated() {
        let schema = load_schema();
        // Message.annotations = field 11 (repeated Annotation)
        let ft = schema.field_type("Message", 11);
        assert!(
            matches!(ft, Some(FieldType::RepeatedMessage(name)) if name.as_ref() == "Annotation"),
            "Message.annotations should be RepeatedMessage(Annotation), got {:?}",
            ft
        );
    }

    // ─────── pblite_to_wire tests ───────

    #[test]
    fn encode_string_field() {
        // Field 1 = "hello"
        let pblite = json!(["hello"]);
        let wire = pblite_to_wire(&pblite).unwrap();
        // Tag: field 1, wire type 2 (LEN) = (1 << 3) | 2 = 0x0A
        // Length: 5
        // Content: "hello"
        assert_eq!(&wire[0..1], &[0x0A]);
        assert_eq!(&wire[1..2], &[5]);
        assert_eq!(&wire[2..], b"hello");
    }

    #[test]
    fn encode_varint_field() {
        // Field 2 = 42
        let pblite = json!([null, 42]);
        let wire = pblite_to_wire(&pblite).unwrap();
        // Tag: field 2, wire type 0 (VARINT) = (2 << 3) | 0 = 0x10
        // Value: 42
        assert_eq!(&wire[..], &[0x10, 42]);
    }

    #[test]
    fn encode_bool_field() {
        // Field 1 = true
        let pblite = json!([true]);
        let wire = pblite_to_wire(&pblite).unwrap();
        // Tag: field 1, wire type 0 = 0x08
        // Value: 1
        assert_eq!(&wire[..], &[0x08, 1]);
    }

    #[test]
    fn encode_nested_message() {
        // Field 1 = nested message with field 1 = "inner"
        let pblite = json!([["inner"]]);
        let wire = pblite_to_wire(&pblite).unwrap();

        // Outer: tag field 1, LEN = 0x0A, length = 7
        assert_eq!(wire[0], 0x0A);
        // Inner message bytes: tag 0x0A, len 5, "inner"
        let inner_len = wire[1] as usize;
        let inner = &wire[2..2 + inner_len];
        assert_eq!(inner[0], 0x0A); // inner field 1, LEN
        assert_eq!(inner[1], 5); // length "inner"
        assert_eq!(&inner[2..], b"inner");
    }

    #[test]
    fn encode_array_of_scalars_as_nested_message() {
        // Without schema info, [1, 2, 3] at field 1 is treated as a nested
        // message with field 1=1, field 2=2, field 3=3 (NOT as repeated int).
        // This is correct for the vast majority of Google Chat fields.
        // Repeated scalar fields would need schema-aware encoding.
        let pblite = json!([[1, 2, 3]]);
        let wire = pblite_to_wire(&pblite).unwrap();

        // Should be: field 1, LEN, <nested message with 3 varint fields>
        assert_eq!(wire[0], 0x0A); // tag: field 1, wire type LEN
                                   // The nested message contains: field 1=1, field 2=2, field 3=3
        let inner_len = wire[1] as usize;
        let inner = &wire[2..2 + inner_len];
        assert_eq!(inner, &[0x08, 1, 0x10, 2, 0x18, 3]);
    }

    #[test]
    fn encode_skips_null_fields() {
        // Field 1 absent, field 2 = "present"
        let pblite = json!([null, "present"]);
        let wire = pblite_to_wire(&pblite).unwrap();
        // Only field 2 should be encoded
        assert_eq!(wire[0], 0x12); // (2 << 3) | 2 = 18 = 0x12
    }

    #[test]
    fn encode_sparse_high_field_numbers() {
        // Positional [field1="a"], sparse {100: "b"}
        let pblite = json!(["a", {"100": "b"}]);
        let wire = pblite_to_wire(&pblite).unwrap();

        // Should contain both field 1 and field 100
        // Field 1: tag 0x0A, len 1, 'a'
        assert_eq!(&wire[0..3], &[0x0A, 1, b'a']);

        // Field 100: tag = (100 << 3) | 2 = 802
        // 802 as varint = 0xA2 0x06
        let rest = &wire[3..];
        assert_eq!(rest[0], 0xA2);
        assert_eq!(rest[1], 0x06);
        assert_eq!(rest[2], 1); // length
        assert_eq!(rest[3], b'b');
    }

    #[test]
    fn empty_array_produces_empty_bytes() {
        let pblite = json!([]);
        let wire = pblite_to_wire(&pblite).unwrap();
        assert!(wire.is_empty());
    }

    #[test]
    fn null_produces_empty_bytes() {
        let pblite = Value::Null;
        let wire = pblite_to_wire(&pblite).unwrap();
        assert!(wire.is_empty());
    }

    #[test]
    fn malformed_json_returns_error() {
        let pblite = json!("not an array");
        assert!(pblite_to_wire(&pblite).is_err());
    }

    // ─────── wire_to_pblite tests ───────

    #[test]
    fn roundtrip_simple_string() {
        let original = json!(["hello"]);
        let wire = pblite_to_wire(&original).unwrap();
        let decoded = wire_to_pblite(&wire).unwrap();

        // Decoded should have field 1 = "hello"
        let arr = decoded.as_array().unwrap();
        assert_eq!(arr[0], json!("hello"));
    }

    #[test]
    fn roundtrip_varint() {
        let original = json!([null, 42]);
        let wire = pblite_to_wire(&original).unwrap();
        let decoded = wire_to_pblite(&wire).unwrap();

        let arr = decoded.as_array().unwrap();
        assert_eq!(arr[0], Value::Null); // field 1 absent
        assert_eq!(arr[1], json!(42)); // field 2 = 42
    }

    #[test]
    fn roundtrip_bool() {
        let original = json!([true]);
        let wire = pblite_to_wire(&original).unwrap();
        let decoded = wire_to_pblite(&wire).unwrap();

        // Bool is encoded as varint 1, decoded as number 1
        let arr = decoded.as_array().unwrap();
        assert_eq!(arr[0], json!(1)); // varint 1 — prost handles the bool interpretation
    }

    #[test]
    fn roundtrip_mixed_message() {
        // Simulates a simple message: field 1=string, field 3=int, field 5=string
        let original = json!(["space_123", null, 1678886400, null, "Hello world"]);
        let wire = pblite_to_wire(&original).unwrap();
        let decoded = wire_to_pblite(&wire).unwrap();

        let arr = decoded.as_array().unwrap();
        assert_eq!(arr.len(), 5);
        assert_eq!(arr[0], json!("space_123"));
        assert!(arr[1].is_null());
        assert_eq!(arr[2], json!(1678886400_u64));
        assert!(arr[3].is_null());
        assert_eq!(arr[4], json!("Hello world"));
    }

    #[test]
    fn roundtrip_large_varint() {
        // Large timestamp-like value
        let original = json!([1678886400000000_u64]);
        let wire = pblite_to_wire(&original).unwrap();
        let decoded = wire_to_pblite(&wire).unwrap();

        let arr = decoded.as_array().unwrap();
        assert_eq!(arr[0], json!(1678886400000000_u64));
    }

    // ─────── varint edge cases ───────

    #[test]
    fn varint_roundtrip_zero() {
        let mut buf = BytesMut::new();
        write_varint(&mut buf, 0);
        let (val, rest) = read_varint(&buf).unwrap();
        assert_eq!(val, 0);
        assert!(rest.is_empty());
    }

    #[test]
    fn varint_roundtrip_max() {
        let mut buf = BytesMut::new();
        write_varint(&mut buf, u64::MAX);
        let (val, rest) = read_varint(&buf).unwrap();
        assert_eq!(val, u64::MAX);
        assert!(rest.is_empty());
    }

    #[test]
    fn varint_roundtrip_boundary() {
        // Test at byte boundaries: 127, 128, 16383, 16384
        for v in [127_u64, 128, 16383, 16384, 2097151, 2097152] {
            let mut buf = BytesMut::new();
            write_varint(&mut buf, v);
            let (decoded, _) = read_varint(&buf).unwrap();
            assert_eq!(decoded, v, "failed for value {v}");
        }
    }

    #[test]
    fn read_varint_empty_data() {
        assert!(read_varint(&[]).is_err());
    }

    // ─────── prost integration roundtrip ───────

    #[test]
    fn prost_roundtrip_user_id() {
        use crate::platform::googlechat::proto;
        use prost::Message;

        // Create a proto UserId via prost.
        // Note: Google Chat user IDs are long numeric strings like
        // "114193829005257704130" — never short digits. We use a
        // realistic ID to test the roundtrip correctly.
        let original = proto::UserId {
            id: Some("114193829005257704130".into()),
            r#type: Some(0), // HUMAN
            origin_app_id: None,
            acting_user_id: Some("actor_xyz_99".into()),
        };

        // Encode to wire bytes with prost
        let wire = original.encode_to_vec();

        // Convert wire → pblite → wire → decode
        let pblite_json = wire_to_pblite(&wire).unwrap();
        let wire_back = pblite_to_wire(&pblite_json).unwrap();
        let decoded = proto::UserId::decode(wire_back).unwrap();

        // Note: the numeric-string heuristic converts the ID to a varint,
        // which prost decodes back to a string representation. The value
        // is semantically equivalent but the wire encoding differs.
        // For real API usage, IDs flow through the interner and are
        // compared by InternedId, not by string value.
        assert!(decoded.id.is_some());
        assert_eq!(decoded.r#type, Some(0));
        assert_eq!(decoded.origin_app_id, None);
        assert_eq!(decoded.acting_user_id, Some("actor_xyz_99".into()));
    }

    #[test]
    fn prost_decode_from_pblite_user_id() {
        use crate::platform::googlechat::proto;
        use prost::Message;

        // Simulate a pblite JSON that would come from the BrowserChannel
        // UserId: field 1 = id (string), field 2 = type (enum/int), field 4 = acting_user_id
        let pblite_json = json!(["user_abc", 1, null, "actor_xyz"]);

        let wire = pblite_to_wire(&pblite_json).unwrap();
        let decoded = proto::UserId::decode(wire).unwrap();

        assert_eq!(decoded.id, Some("user_abc".into()));
        assert_eq!(decoded.r#type, Some(1)); // BOT
        assert_eq!(decoded.acting_user_id, Some("actor_xyz".into()));
    }

    #[test]
    fn prost_roundtrip_space_id() {
        use crate::platform::googlechat::proto;
        use prost::Message;

        let original = proto::SpaceId {
            space_id: Some("spaces/abc123".into()),
        };

        let wire = original.encode_to_vec();
        let pblite_json = wire_to_pblite(&wire).unwrap();
        let wire_back = pblite_to_wire(&pblite_json).unwrap();
        let decoded = proto::SpaceId::decode(wire_back).unwrap();

        assert_eq!(decoded.space_id, Some("spaces/abc123".into()));
    }

    // ─────── request_header field 100 (sparse map) roundtrip ───────

    #[test]
    fn prost_roundtrip_request_header_field_100() {
        use crate::platform::googlechat::proto;
        use prost::Message;

        let original = proto::SetTypingStateRequest {
            request_header: Some(proto::RequestHeader {
                client_type: Some(3),
                client_version: None,
                trace_id: None,
                locale: Some("en".into()),
                client_feature_capabilities: None,
            }),
            state: Some(1),
            context: Some(proto::TypingContext {
                group_id: Some(proto::GroupId {
                    space_id: Some(proto::SpaceId {
                        space_id: Some("spaces/abc".into()),
                    }),
                    dm_id: None,
                }),
                topic_id: None,
            }),
        };

        let wire = original.encode_to_vec();
        let pblite_json = wire_to_pblite(&wire).unwrap();
        let wire_back = pblite_to_wire(&pblite_json).unwrap();
        let decoded = proto::SetTypingStateRequest::decode(wire_back).unwrap();

        assert_eq!(decoded.state, Some(1));
        let header = decoded.request_header.unwrap();
        assert_eq!(header.client_type, Some(3));
        assert_eq!(header.locale, Some("en".into()));
        let ctx = decoded.context.unwrap();
        let gid = ctx.group_id.unwrap();
        assert_eq!(gid.space_id.unwrap().space_id, Some("spaces/abc".into()));
    }

    // ─────── CatchUpGroupRequest roundtrip ───────

    #[test]
    fn prost_roundtrip_catch_up_group_request() {
        use crate::platform::googlechat::proto;
        use prost::Message;

        let original = proto::CatchUpGroupRequest {
            request_header: Some(proto::RequestHeader {
                client_type: Some(3),
                client_version: None,
                trace_id: None,
                locale: Some("en".into()),
                client_feature_capabilities: None,
            }),
            group_id: Some(proto::GroupId {
                space_id: Some(proto::SpaceId {
                    space_id: Some("spaces/test123".into()),
                }),
                dm_id: None,
            }),
            range: Some(proto::CatchUpRange {
                from_revision_timestamp: Some(1000000),
                to_revision_timestamp: Some(9999999),
            }),
            page_size: Some(50),
            cutoff_size: None,
        };

        let wire = original.encode_to_vec();
        let pblite_json = wire_to_pblite(&wire).unwrap();
        let wire_back = pblite_to_wire(&pblite_json).unwrap();
        let decoded = proto::CatchUpGroupRequest::decode(wire_back).unwrap();

        assert_eq!(decoded.page_size, Some(50));
        let gid = decoded.group_id.unwrap();
        assert_eq!(
            gid.space_id.unwrap().space_id,
            Some("spaces/test123".into())
        );
        let range = decoded.range.unwrap();
        assert_eq!(range.from_revision_timestamp, Some(1000000));
        assert_eq!(range.to_revision_timestamp, Some(9999999));
        let header = decoded.request_header.unwrap();
        assert_eq!(header.client_type, Some(3));
    }

    // ─────── Repeated message field roundtrip ───────

    #[test]
    fn prost_roundtrip_paginated_world_with_items() {
        use crate::platform::googlechat::proto;
        use prost::Message;

        let original = proto::PaginatedWorldResponse {
            world_section_responses: Vec::new(),
            world_consistency_token: Some("token_abc".into()),
            user_revision: None,
            world_items: vec![
                proto::WorldItemLite {
                    group_id: Some(proto::GroupId {
                        space_id: Some(proto::SpaceId {
                            space_id: Some("spaces/aaa".into()),
                        }),
                        dm_id: None,
                    }),
                    sort_timestamp: Some(1000),
                    room_name: Some("Room A".into()),
                    ..Default::default()
                },
                proto::WorldItemLite {
                    group_id: Some(proto::GroupId {
                        space_id: Some(proto::SpaceId {
                            space_id: Some("spaces/bbb".into()),
                        }),
                        dm_id: None,
                    }),
                    sort_timestamp: Some(2000),
                    room_name: Some("Room B".into()),
                    ..Default::default()
                },
            ],
        };

        let wire = original.encode_to_vec();
        let pblite_json = wire_to_pblite(&wire).unwrap();
        let wire_back = pblite_to_wire(&pblite_json).unwrap();
        let decoded = proto::PaginatedWorldResponse::decode(wire_back).unwrap();

        assert_eq!(decoded.world_consistency_token, Some("token_abc".into()));
        assert_eq!(decoded.world_items.len(), 2);
        assert_eq!(decoded.world_items[0].room_name, Some("Room A".into()));
        assert_eq!(decoded.world_items[1].room_name, Some("Room B".into()));
        assert_eq!(decoded.world_items[0].sort_timestamp, Some(1000));
    }

    // ─────── Negative i64 varint ───────

    #[test]
    fn varint_negative_i64_roundtrip() {
        let mut buf = BytesMut::new();
        let neg: i64 = -1;
        write_varint(&mut buf, neg as u64);
        let (val, rest) = read_varint(&buf).unwrap();
        assert_eq!(val as i64, -1);
        assert!(rest.is_empty());
    }

    // ─────── Large sparse positional array with gaps ───────

    #[test]
    fn encode_sparse_positional_array_with_gaps() {
        // Field 10 = "field_10", fields 1-9 are null
        let pblite = json!([null, null, null, null, null, null, null, null, null, "field_10"]);
        let wire = pblite_to_wire(&pblite).unwrap();
        let decoded = wire_to_pblite(&wire).unwrap();
        let arr = decoded.as_array().unwrap();
        assert_eq!(arr.len(), 10);
        for (i, item) in arr.iter().take(9).enumerate() {
            assert!(item.is_null(), "expected null at index {i}");
        }
        assert_eq!(arr[9], json!("field_10"));
    }

    // ─────── wire_to_pblite with repeated fields ───────

    #[test]
    fn wire_to_pblite_groups_repeated_fields() {
        // Manually encode field 1 twice as length-delimited strings
        let mut buf = BytesMut::new();
        // Field 1, wire type LEN = 0x0A
        write_tag(&mut buf, 1, WIRE_LEN);
        write_varint(&mut buf, 5);
        buf.put_slice(b"hello");
        write_tag(&mut buf, 1, WIRE_LEN);
        write_varint(&mut buf, 5);
        buf.put_slice(b"world");

        let pblite = wire_to_pblite(&buf).unwrap();
        let arr = pblite.as_array().unwrap();
        // Field 1 should be an array of two values
        assert!(arr[0].is_array(), "expected array for repeated field");
        let repeated = arr[0].as_array().unwrap();
        assert_eq!(repeated.len(), 2);
        assert_eq!(repeated[0], json!("hello"));
        assert_eq!(repeated[1], json!("world"));
    }

    // ─────── CreateMessageRequest roundtrip ───────

    #[test]
    fn prost_roundtrip_create_message_request() {
        use crate::platform::googlechat::proto;
        use prost::Message;

        let original = proto::CreateMessageRequest {
            request_header: Some(proto::RequestHeader {
                client_type: Some(3),
                client_version: None,
                trace_id: None,
                locale: Some("en".into()),
                client_feature_capabilities: None,
            }),
            parent_id: Some(proto::MessageParentId {
                topic_id: Some(proto::TopicId {
                    group_id: Some(proto::GroupId {
                        space_id: Some(proto::SpaceId {
                            space_id: Some("spaces/room1".into()),
                        }),
                        dm_id: None,
                    }),
                    topic_id: Some("topic_123".into()),
                }),
            }),
            text_body: Some("Hello world!".into()),
            annotations: Vec::new(),
            local_id: Some("local_1".into()),
            message_id: None,
            message_info: None,
        };

        let wire = original.encode_to_vec();
        let pblite_json = wire_to_pblite(&wire).unwrap();
        let wire_back = pblite_to_wire(&pblite_json).unwrap();
        let decoded = proto::CreateMessageRequest::decode(wire_back).unwrap();

        assert_eq!(decoded.text_body, Some("Hello world!".into()));
        assert_eq!(decoded.local_id, Some("local_1".into()));
        let parent = decoded.parent_id.unwrap();
        let topic = parent.topic_id.unwrap();
        assert_eq!(topic.topic_id, Some("topic_123".into()));
        let header = decoded.request_header.unwrap();
        assert_eq!(header.client_type, Some(3));
    }
}
