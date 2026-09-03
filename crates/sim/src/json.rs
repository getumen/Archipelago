//! A minimal, hand-written JSON `Value` type with a parser and a
//! serializer. No external crate (docs/phase5-spec.md §0: std-only server),
//! following the precedent already set by `apps/headless/src/json.rs` -
//! this module additionally *parses*, since both `crate::scenario` (reading
//! a `--scenario` file) and the API (reading whatever JSON a hostile RL
//! agent sends it) need a real parser, never just a writer.
//!
//! Stage 6A (docs/phase6-spec.md "Stage 6A"): this used to live in
//! `crates/api/src/json.rs`. It moved here so `crates/sim::scenario` can
//! parse scenario files without `crates/sim` gaining a dependency on
//! `crates/api` (the wrong direction - `crates/api` already depends on
//! `crates/sim`, never the reverse) and without a second hand-written
//! parser existing anywhere in the workspace. `crates/api/src/json.rs` now
//! just re-exports this module, so every existing `crate::json::Value`/
//! `crate::json::parse` call site in that crate keeps compiling unchanged.
//!
//! The parser never panics: every malformed byte sequence becomes
//! `Err(JsonError)`, which callers turn into their own error type (the API
//! layer's 400 response with a reason; `scenario::ScenarioError::Json` for
//! a malformed scenario file) - exactly the same "never crash on hostile
//! input" discipline `archipelago_sim::action` already applies to `Action`.

use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    /// A JSON number literal already formatted exactly as `f32`'s own
    /// `Display` would render it - see `Value::f32num`'s doc for why this
    /// is kept distinct from `Number(f64)` rather than just widening every
    /// `f32` into an `f64` before storing it.
    Raw(String),
    String(String),
    Array(Vec<Value>),
    /// `BTreeMap` (not `HashMap`) purely so serializing a `Value` we built
    /// ourselves is deterministic byte-for-byte - nothing in this crate
    /// depends on parsed-object key order, since callers look fields up by
    /// name.
    Object(BTreeMap<String, Value>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct JsonError(pub String);

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid JSON: {}", self.0)
    }
}

impl Value {
    pub fn as_object(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Value::Object(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            Value::Raw(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Every integer-typed field on the wire (`seed`, `faction`, `steps`,
    /// region/unit/faction ids, ...) is documented as accepting an integer,
    /// so `{"seed":1.9}` must not be silently accepted as `1` - that would
    /// let a caller ask for one thing and have a materially different value
    /// applied. Requires `fract() == 0.0` before truncating, the same fix
    /// already applied to the LLM doctrine parser's `to_faction`/`to_region`
    /// (`crates/agents/src/llm.rs`) for exactly this defect shape.
    pub fn as_u64(&self) -> Option<u64> {
        self.as_f64().filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0).map(|n| n as u64)
    }

    /// See `as_u64`'s doc - same integrality requirement, bounded to `u32`.
    pub fn as_u32(&self) -> Option<u32> {
        self.as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0 && *n <= u32::MAX as f64 && n.fract() == 0.0)
            .map(|n| n as u32)
    }

    /// Narrows to `f32`, requiring the *result* to be finite - not just the
    /// `f64` this was parsed as. A finite `f64` can still overflow `f32`'s
    /// much smaller range (e.g. `1e100` is a perfectly finite `f64` but
    /// becomes `f32::INFINITY` once narrowed), so checking finiteness before
    /// the `as f32` cast lets exactly that value pass validation and then
    /// silently turn into an infinity everywhere this crate actually uses
    /// it as `f32` (every simulation field this parses into is `f32`).
    /// Checking after the cast is what actually catches it.
    pub fn as_f32(&self) -> Option<f32> {
        self.as_f64().map(|n| n as f32).filter(|n| n.is_finite())
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Field lookup on an object, `None` for anything else (including a
    /// missing key) - the caller decides whether a missing field is an
    /// error.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object().and_then(|m| m.get(key))
    }

    pub fn obj(pairs: Vec<(&str, Value)>) -> Value {
        Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    pub fn arr(items: Vec<Value>) -> Value {
        Value::Array(items)
    }

    pub fn str(s: impl Into<String>) -> Value {
        Value::String(s.into())
    }

    pub fn num(n: impl Into<f64>) -> Value {
        Value::Number(n.into())
    }

    /// A JSON number rendered exactly as `apps/headless/src/json.rs`'s own
    /// `number()` would render this same `f32` - i.e. via `f32`'s `Display`
    /// directly, never widened through `f64` first. Widening an `f32`
    /// through `as f64` before formatting is *not* just cosmetic: an `f32`
    /// generally isn't exactly representable in decimal, so its nearest
    /// `f64` can have a much longer minimal decimal expansion than the
    /// `f32` itself does (e.g. the `f32` nearest `0.1` prints as `"0.1"`
    /// directly, but as `"0.10000000149011612"` once widened to `f64` and
    /// formatted there) - which would make every non-integer field in this
    /// crate's JSON output disagree, byte-for-byte, with the identical
    /// value `apps/headless --json` prints for it. Every simulation number
    /// this crate ever serializes originates as `f32`
    /// (`archipelago_sim::world`'s fields, `Observation::encode()`), so
    /// this - not `num` - is what every such call site uses.
    pub fn f32num(v: f32) -> Value {
        if v.is_finite() { Value::Raw(format!("{v}")) } else { Value::Raw("0".to_string()) }
    }

    /// Renders as compact JSON. Every `f64`/`f32` this crate ever puts into
    /// a `Value` is either an integer-valued count/id or a value already
    /// clamped finite by the simulation, so a non-finite number here is
    /// rendered as `0` rather than emitting invalid JSON (mirrors
    /// `apps/headless/src/json.rs::number`).
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out);
        out
    }

    fn write_json(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => {
                if n.is_finite() {
                    if *n == n.trunc() && n.abs() < 1e15 {
                        out.push_str(&format!("{}", *n as i64));
                    } else {
                        out.push_str(&format!("{n}"));
                    }
                } else {
                    out.push('0');
                }
            }
            Value::Raw(s) => out.push_str(s),
            Value::String(s) => write_json_string(s, out),
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write_json(out);
                }
                out.push(']');
            }
            Value::Object(map) => {
                out.push('{');
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_json_string(k, out);
                    out.push(':');
                    v.write_json(out);
                }
                out.push('}');
            }
        }
    }
}

fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Parses `input` as a single JSON value, requiring the whole (trimmed)
/// input be consumed - a hostile client appending garbage after a
/// syntactically valid document is rejected, not silently truncated.
///
/// `max_depth` bounds array/object nesting so a client can't crash this
/// process with a few kilobytes of `[[[[[...`. Recursion depth is what
/// actually matters (a deeply nested document can blow the stack well
/// before it blows any byte-size limit), so this is enforced independently
/// of the request-size cap the HTTP layer applies.
pub fn parse(input: &str, max_depth: u32) -> Result<Value, JsonError> {
    let bytes = input.as_bytes();
    let mut pos = 0usize;
    skip_ws(bytes, &mut pos);
    let value = parse_value(bytes, &mut pos, max_depth, 0)?;
    skip_ws(bytes, &mut pos);
    if pos != bytes.len() {
        return Err(JsonError(format!("trailing data at byte {pos}")));
    }
    Ok(value)
}

fn skip_ws(bytes: &[u8], pos: &mut usize) {
    while *pos < bytes.len() && matches!(bytes[*pos], b' ' | b'\t' | b'\n' | b'\r') {
        *pos += 1;
    }
}

fn parse_value(bytes: &[u8], pos: &mut usize, max_depth: u32, depth: u32) -> Result<Value, JsonError> {
    if depth > max_depth {
        return Err(JsonError("nesting too deep".to_string()));
    }
    skip_ws(bytes, pos);
    let Some(&b) = bytes.get(*pos) else {
        return Err(JsonError("unexpected end of input".to_string()));
    };
    match b {
        b'{' => parse_object(bytes, pos, max_depth, depth),
        b'[' => parse_array(bytes, pos, max_depth, depth),
        b'"' => parse_string(bytes, pos).map(Value::String),
        b't' => parse_literal(bytes, pos, "true", Value::Bool(true)),
        b'f' => parse_literal(bytes, pos, "false", Value::Bool(false)),
        b'n' => parse_literal(bytes, pos, "null", Value::Null),
        b'-' | b'0'..=b'9' => parse_number(bytes, pos),
        other => Err(JsonError(format!("unexpected byte {other:#x} at {}", *pos))),
    }
}

fn parse_literal(bytes: &[u8], pos: &mut usize, lit: &str, value: Value) -> Result<Value, JsonError> {
    let end = *pos + lit.len();
    if end > bytes.len() || &bytes[*pos..end] != lit.as_bytes() {
        return Err(JsonError(format!("expected `{lit}` at byte {}", *pos)));
    }
    *pos = end;
    Ok(value)
}

fn parse_object(bytes: &[u8], pos: &mut usize, max_depth: u32, depth: u32) -> Result<Value, JsonError> {
    *pos += 1; // '{'
    let mut map = BTreeMap::new();
    skip_ws(bytes, pos);
    if bytes.get(*pos) == Some(&b'}') {
        *pos += 1;
        return Ok(Value::Object(map));
    }
    loop {
        skip_ws(bytes, pos);
        if bytes.get(*pos) != Some(&b'"') {
            return Err(JsonError(format!("expected object key at byte {}", *pos)));
        }
        let key = parse_string(bytes, pos)?;
        skip_ws(bytes, pos);
        if bytes.get(*pos) != Some(&b':') {
            return Err(JsonError(format!("expected ':' at byte {}", *pos)));
        }
        *pos += 1;
        let value = parse_value(bytes, pos, max_depth, depth + 1)?;
        map.insert(key, value);
        skip_ws(bytes, pos);
        match bytes.get(*pos) {
            Some(b',') => {
                *pos += 1;
            }
            Some(b'}') => {
                *pos += 1;
                break;
            }
            _ => return Err(JsonError(format!("expected ',' or '}}' at byte {}", *pos))),
        }
    }
    Ok(Value::Object(map))
}

fn parse_array(bytes: &[u8], pos: &mut usize, max_depth: u32, depth: u32) -> Result<Value, JsonError> {
    *pos += 1; // '['
    let mut items = Vec::new();
    skip_ws(bytes, pos);
    if bytes.get(*pos) == Some(&b']') {
        *pos += 1;
        return Ok(Value::Array(items));
    }
    loop {
        let value = parse_value(bytes, pos, max_depth, depth + 1)?;
        items.push(value);
        skip_ws(bytes, pos);
        match bytes.get(*pos) {
            Some(b',') => {
                *pos += 1;
            }
            Some(b']') => {
                *pos += 1;
                break;
            }
            _ => return Err(JsonError(format!("expected ',' or ']' at byte {}", *pos))),
        }
    }
    Ok(Value::Array(items))
}

fn parse_string(bytes: &[u8], pos: &mut usize) -> Result<String, JsonError> {
    *pos += 1; // opening quote
    let mut out = String::new();
    loop {
        let Some(&b) = bytes.get(*pos) else {
            return Err(JsonError("unterminated string".to_string()));
        };
        match b {
            b'"' => {
                *pos += 1;
                return Ok(out);
            }
            b'\\' => {
                *pos += 1;
                let Some(&esc) = bytes.get(*pos) else {
                    return Err(JsonError("unterminated escape".to_string()));
                };
                match esc {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'n' => out.push('\n'),
                    b't' => out.push('\t'),
                    b'r' => out.push('\r'),
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'u' => {
                        let cp = parse_hex4(bytes, *pos + 1)?;
                        *pos += 4;
                        // Not attempting surrogate-pair reassembly - good
                        // enough for the ASCII identifiers/text this API
                        // actually needs to parse, and a lone surrogate
                        // becomes the Unicode replacement character rather
                        // than a panic.
                        out.push(char::from_u32(cp as u32).unwrap_or('\u{fffd}'));
                    }
                    other => return Err(JsonError(format!("invalid escape \\{}", other as char))),
                }
                *pos += 1;
            }
            0..=0x1f => return Err(JsonError("control character in string".to_string())),
            _ => {
                // Copy one UTF-8 codepoint's worth of bytes at a time so we
                // never split a multi-byte sequence.
                let start = *pos;
                let width = utf8_width(b);
                let end = start + width;
                if end > bytes.len() {
                    return Err(JsonError("truncated UTF-8 in string".to_string()));
                }
                let chunk = std::str::from_utf8(&bytes[start..end])
                    .map_err(|_| JsonError("invalid UTF-8 in string".to_string()))?;
                out.push_str(chunk);
                *pos = end;
            }
        }
    }
}

fn utf8_width(lead: u8) -> usize {
    if lead & 0x80 == 0 {
        1
    } else if lead & 0xE0 == 0xC0 {
        2
    } else if lead & 0xF0 == 0xE0 {
        3
    } else {
        4
    }
}

fn parse_hex4(bytes: &[u8], start: usize) -> Result<u16, JsonError> {
    let slice = bytes.get(start..start + 4).ok_or_else(|| JsonError("truncated \\u escape".to_string()))?;
    let s = std::str::from_utf8(slice).map_err(|_| JsonError("invalid \\u escape".to_string()))?;
    u16::from_str_radix(s, 16).map_err(|_| JsonError("invalid \\u escape".to_string()))
}

fn parse_number(bytes: &[u8], pos: &mut usize) -> Result<Value, JsonError> {
    let start = *pos;
    if bytes.get(*pos) == Some(&b'-') {
        *pos += 1;
    }
    while matches!(bytes.get(*pos), Some(b'0'..=b'9')) {
        *pos += 1;
    }
    if bytes.get(*pos) == Some(&b'.') {
        *pos += 1;
        while matches!(bytes.get(*pos), Some(b'0'..=b'9')) {
            *pos += 1;
        }
    }
    if matches!(bytes.get(*pos), Some(b'e') | Some(b'E')) {
        *pos += 1;
        if matches!(bytes.get(*pos), Some(b'+') | Some(b'-')) {
            *pos += 1;
        }
        while matches!(bytes.get(*pos), Some(b'0'..=b'9')) {
            *pos += 1;
        }
    }
    let s = std::str::from_utf8(&bytes[start..*pos]).map_err(|_| JsonError("invalid number".to_string()))?;
    s.parse::<f64>().map(Value::Number).map_err(|_| JsonError(format!("invalid number literal `{s}`")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_basic_values() {
        let v = parse(r#"{"a":1,"b":[true,false,null,"x\ny"],"c":-1.5e2}"#, 32).unwrap();
        assert_eq!(v.get("a").unwrap().as_u64(), Some(1));
        assert_eq!(v.get("b").unwrap().as_array().unwrap().len(), 4);
        assert_eq!(v.get("c").unwrap().as_f64(), Some(-150.0));
    }

    #[test]
    fn rejects_trailing_garbage() {
        assert!(parse("{}garbage", 32).is_err());
    }

    #[test]
    fn rejects_deep_nesting() {
        let deep = "[".repeat(10_000);
        assert!(parse(&deep, 64).is_err());
    }

    #[test]
    fn rejects_malformed_input_without_panicking() {
        for bad in ["", "{", "[1,2", "\"unterminated", "{\"a\":}", "nul", "12x"] {
            assert!(parse(bad, 32).is_err(), "expected error for {bad:?}");
        }
    }
}
