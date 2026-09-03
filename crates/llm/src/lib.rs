//! `HttpBackend`: an `archipelago_agents::llm::LlmBackend` that actually
//! calls out to a real LLM server (docs/phase4-spec.md "Stage 4A" backend
//! table: "`HttpBackend` | 実際の LLM API を叩く。別クレート crates/llm に
//! 置き、crates/agents はトレイトだけに依存する").
//!
//! ## Why this crate exists and why nothing else depends on it
//!
//! `crates/sim` and `crates/agents` stay dependency-free and fully
//! deterministic (docs/phase4-spec.md §0) - the only thing in this whole
//! workspace allowed to reach out to the network is this crate, and nothing
//! else in the workspace (not `archipelago-agents`, not `archipelago-
//! headless`) depends on it. Deleting `crates/llm` entirely - or building
//! with `cargo build --workspace --exclude archipelago-llm` - leaves every
//! other crate exactly as buildable and testable as before, `MockBackend`/
//! `ScriptedBackend` included. No test in this crate, or anywhere else in
//! the workspace, opens a real socket - see this module's own tests, which
//! only exercise the pure request-formatting and response-parsing helpers.
//!
//! ## Why plain HTTP over `std::net::TcpStream`, not an HTTPS client crate
//!
//! docs/phase4-spec.md asks this to be considered first: "MVP では標準ライブ
//! ラリのみで組めるかを先に検討し、依存が必要なら crates/llm に限定し...".
//! A correct, safe TLS implementation is not something to hand-roll, so this
//! backend speaks **plain HTTP with no TLS** - it can reach a local or
//! proxied endpoint only (loopback, a private network, or a
//! `kubectl port-forward`/reverse-proxy standing in front of something
//! else). It cannot reach a TLS-only hosted provider (e.g. `api.openai.com`)
//! directly, and never will without adding a TLS dependency to this crate.
//! It targets self-hosted inference servers that commonly listen on plain
//! HTTP: Ollama, llama.cpp's `server`, and vLLM, all of which expose an
//! OpenAI-compatible `POST /v1/chat/completions` endpoint - a configurable
//! `host:port` and path, one POST per consult, `Connection: close` so a
//! full response can be read with a single `read_to_end` - and stays at
//! zero external dependencies for the *entire* workspace, not just for
//! `crates/sim`/`crates/agents`. Reaching a TLS-only provider directly is
//! left to a reverse proxy in front of it, or to swapping in a TLS crate
//! here later behind this same `LlmBackend` trait - the trait boundary is
//! exactly what makes that swap not touch `crates/agents` or `crates/sim`
//! at all.
//!
//! ## Request/response shape
//!
//! The request body is the OpenAI-compatible chat-completions shape that
//! Ollama, llama.cpp's `server` and vLLM all accept on their
//! `/v1/chat/completions` endpoint: a JSON object with `model`, a
//! `messages` array holding one `system` and one `user` message, and
//! `max_tokens`. `model` and the request path are configurable on
//! `HttpBackend` (see `HttpBackend::new`), not hardcoded, since which model
//! name a given server expects varies by deployment.
//!
//! The response is expected to be that same family's response envelope:
//! `{"choices":[{"message":{"content":"..."}}], ...}`. This backend parses
//! that envelope itself (see `extract_message_content`) and hands the
//! extracted assistant message content - not the raw HTTP body - to the
//! caller, which in turn feeds it to
//! `archipelago_agents::llm::parse_doctrine`'s tolerant "find the first
//! balanced `{...}`" extraction to pull the `Doctrine` JSON out of whatever
//! the model actually said. A response envelope that doesn't match this
//! shape becomes `LlmError::Malformed`, exactly like a `Doctrine` that fails
//! to parse - never a panic.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use archipelago_agents::llm::{LlmBackend, LlmError, LlmRequest};

/// Talks to a single LLM server over plain HTTP/1.1, speaking the
/// OpenAI-compatible chat-completions shape (see this module's doc for what
/// that means and what it can/can't reach).
pub struct HttpBackend {
    host: String,
    port: u16,
    path: String,
    model: String,
    timeout: Duration,
}

impl HttpBackend {
    /// `host` is used both for the TCP connection and the `Host` header (no
    /// separate TLS server-name concern here, since there is no TLS).
    /// `path` is typically `/v1/chat/completions` (Ollama, llama.cpp's
    /// `server` and vLLM all serve that path); `model` is whatever name the
    /// target server expects in the request body's `"model"` field -
    /// neither is hardcoded, since both vary by deployment.
    pub fn new(
        host: impl Into<String>,
        port: u16,
        path: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        HttpBackend {
            host: host.into(),
            port,
            path: path.into(),
            model: model.into(),
            timeout: Duration::from_secs(20),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl LlmBackend for HttpBackend {
    fn name(&self) -> &str {
        "HttpBackend"
    }

    /// One request/response round trip. Every failure mode - connect,
    /// write, read, an HTTP-level error status, or a response body whose
    /// envelope isn't what was expected - becomes an `LlmError`, never a
    /// panic, so `LlmAgent`'s fallback semantics apply exactly the same way
    /// they do for `MockBackend`/`ScriptedBackend`.
    fn complete(&self, request: &LlmRequest) -> Result<String, LlmError> {
        let body = build_request_json(request, &self.model);

        let mut stream = TcpStream::connect((self.host.as_str(), self.port))
            .map_err(|e| LlmError::Backend(format!("connect to {}:{} failed: {e}", self.host, self.port)))?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|e| LlmError::Backend(format!("set_read_timeout failed: {e}")))?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(|e| LlmError::Backend(format!("set_write_timeout failed: {e}")))?;

        let http_request = format!(
            "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
            path = self.path,
            host = self.host,
            len = body.len(),
            body = body,
        );
        stream.write_all(http_request.as_bytes()).map_err(io_error_to_llm_error)?;

        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).map_err(io_error_to_llm_error)?;
        let text = String::from_utf8_lossy(&raw).into_owned();

        let (status, resp_body) = split_http_response(&text)
            .ok_or_else(|| LlmError::Backend("response had no HTTP header/body separator".to_string()))?;
        if !(200..300).contains(&status) {
            return Err(LlmError::Backend(format!("HTTP status {status}")));
        }
        extract_message_content(resp_body)
    }
}

fn io_error_to_llm_error(e: std::io::Error) -> LlmError {
    match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => LlmError::Timeout,
        _ => LlmError::Backend(e.to_string()),
    }
}

/// Splits a raw HTTP/1.1 response into its status code and body. Deliberately
/// minimal: assumes `Connection: close` (so the body is everything after the
/// blank line, with no chunked-transfer-encoding or additional-response
/// handling) - correct for the request this backend always sends, not a
/// general-purpose HTTP client.
fn split_http_response(response: &str) -> Option<(u16, &str)> {
    let (head, body) = response.split_once("\r\n\r\n")?;
    let status_line = head.lines().next()?;
    let mut parts = status_line.split_whitespace();
    parts.next()?; // "HTTP/1.1"
    let status: u16 = parts.next()?.parse().ok()?;
    Some((status, body))
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
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
    out
}

/// Builds the OpenAI-compatible chat-completions request body: `model`, a
/// `messages` array with one `system` and one `user` message (in that
/// order), and `max_tokens` - the shape Ollama's, llama.cpp's `server`'s and
/// vLLM's `/v1/chat/completions` endpoints all expect. Earlier versions of
/// this backend sent a bespoke `{"system","user","max_output_tokens"}` body
/// that none of those servers understand at all.
fn build_request_json(request: &LlmRequest, model: &str) -> String {
    format!(
        "{{\"model\":\"{model}\",\"messages\":[{{\"role\":\"system\",\"content\":\"{system}\"}},{{\"role\":\"user\",\"content\":\"{user}\"}}],\"max_tokens\":{max_tokens}}}",
        model = json_escape(model),
        system = json_escape(&request.system),
        user = json_escape(&request.user),
        max_tokens = request.max_output_tokens,
    )
}

// ---------------------------------------------------------------------
// Minimal hand-written JSON parsing for the response envelope only (no
// serde, no external crate - docs/phase4-spec.md's "外部依存 0" rule applies
// to this crate too). Deliberately smaller than
// `archipelago_agents::llm`'s parser: it only needs to walk down to
// `choices[0].message.content`, so booleans/numbers/nulls anywhere else in
// the envelope just need to be skipped over correctly, never interpreted.
// ---------------------------------------------------------------------

#[derive(Clone, Debug)]
enum JsonValue {
    Null,
    // Neither of these is ever read back out by `extract_message_content`
    // (the envelope's payload of interest is always a string), but both are
    // kept as full values - not reduced to unit variants - so the parser
    // still walks past a `"usage":{"total_tokens":42}` or similar sibling
    // field correctly rather than treating it as a syntax error. Read in
    // this module's own tests (`http_request_body_is_openai_compatible`),
    // hence `#[allow(dead_code)]` rather than deleting the payload.
    #[allow(dead_code)]
    Bool(bool),
    #[allow(dead_code)]
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

struct JsonParser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> JsonParser<'a> {
    fn new(s: &'a str) -> Self {
        JsonParser { chars: s.chars().peekable() }
    }

    fn skip_ws(&mut self) {
        while matches!(self.chars.peek(), Some(c) if c.is_whitespace()) {
            self.chars.next();
        }
    }

    fn peek_char(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }

    fn expect(&mut self, c: char) -> Result<(), ()> {
        if self.chars.next() == Some(c) {
            Ok(())
        } else {
            Err(())
        }
    }

    fn parse_value(&mut self) -> Result<JsonValue, ()> {
        self.skip_ws();
        match self.peek_char() {
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
            Some('"') => self.parse_string().map(JsonValue::String),
            Some('t') => self.parse_literal("true", JsonValue::Bool(true)),
            Some('f') => self.parse_literal("false", JsonValue::Bool(false)),
            Some('n') => self.parse_literal("null", JsonValue::Null),
            Some(c) if c == '-' || c.is_ascii_digit() => self.parse_number(),
            _ => Err(()),
        }
    }

    fn parse_literal(&mut self, lit: &str, value: JsonValue) -> Result<JsonValue, ()> {
        for expected in lit.chars() {
            if self.chars.next() != Some(expected) {
                return Err(());
            }
        }
        Ok(value)
    }

    fn parse_object(&mut self) -> Result<JsonValue, ()> {
        self.expect('{')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek_char() == Some('}') {
            self.chars.next();
            return Ok(JsonValue::Object(items));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(':')?;
            let value = self.parse_value()?;
            items.push((key, value));
            self.skip_ws();
            match self.chars.next() {
                Some(',') => continue,
                Some('}') => break,
                _ => return Err(()),
            }
        }
        Ok(JsonValue::Object(items))
    }

    fn parse_array(&mut self) -> Result<JsonValue, ()> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek_char() == Some(']') {
            self.chars.next();
            return Ok(JsonValue::Array(items));
        }
        loop {
            let value = self.parse_value()?;
            items.push(value);
            self.skip_ws();
            match self.chars.next() {
                Some(',') => continue,
                Some(']') => break,
                _ => return Err(()),
            }
        }
        Ok(JsonValue::Array(items))
    }

    fn parse_string(&mut self) -> Result<String, ()> {
        self.skip_ws();
        self.expect('"')?;
        let mut out = String::new();
        loop {
            match self.chars.next() {
                None => return Err(()),
                Some('"') => break,
                Some('\\') => match self.chars.next() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some('b') => out.push('\u{8}'),
                    Some('f') => out.push('\u{c}'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('u') => {
                        let cp = self.parse_hex4()?;
                        if (0xD800..=0xDBFF).contains(&cp) {
                            if self.chars.next() != Some('\\') || self.chars.next() != Some('u') {
                                return Err(());
                            }
                            let low = self.parse_hex4()?;
                            if !(0xDC00..=0xDFFF).contains(&low) {
                                return Err(());
                            }
                            let combined = 0x10000 + ((cp - 0xD800) << 10) + (low - 0xDC00);
                            out.push(char::from_u32(combined).ok_or(())?);
                        } else {
                            out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                        }
                    }
                    _ => return Err(()),
                },
                Some(c) => out.push(c),
            }
        }
        Ok(out)
    }

    fn parse_hex4(&mut self) -> Result<u32, ()> {
        let mut v: u32 = 0;
        for _ in 0..4 {
            let c = self.chars.next().ok_or(())?;
            let d = c.to_digit(16).ok_or(())?;
            v = v * 16 + d;
        }
        Ok(v)
    }

    fn parse_number(&mut self) -> Result<JsonValue, ()> {
        let mut s = String::new();
        if self.peek_char() == Some('-') {
            s.push('-');
            self.chars.next();
        }
        match self.peek_char() {
            Some('0') => {
                s.push('0');
                self.chars.next();
            }
            Some(c) if c.is_ascii_digit() => {
                while matches!(self.peek_char(), Some(c) if c.is_ascii_digit()) {
                    s.push(self.chars.next().unwrap());
                }
            }
            _ => return Err(()),
        }
        if self.peek_char() == Some('.') {
            s.push('.');
            self.chars.next();
            if !matches!(self.peek_char(), Some(c) if c.is_ascii_digit()) {
                return Err(());
            }
            while matches!(self.peek_char(), Some(c) if c.is_ascii_digit()) {
                s.push(self.chars.next().unwrap());
            }
        }
        if matches!(self.peek_char(), Some('e') | Some('E')) {
            s.push(self.chars.next().unwrap());
            if matches!(self.peek_char(), Some('+') | Some('-')) {
                s.push(self.chars.next().unwrap());
            }
            if !matches!(self.peek_char(), Some(c) if c.is_ascii_digit()) {
                return Err(());
            }
            while matches!(self.peek_char(), Some(c) if c.is_ascii_digit()) {
                s.push(self.chars.next().unwrap());
            }
        }
        s.parse::<f64>().map(JsonValue::Number).map_err(|_| ())
    }
}

fn parse_json(s: &str) -> Result<JsonValue, ()> {
    let mut parser = JsonParser::new(s);
    let value = parser.parse_value()?;
    parser.skip_ws();
    if parser.chars.next().is_some() {
        return Err(()); // trailing garbage after the value
    }
    Ok(value)
}

fn object_get<'a>(fields: &'a [(String, JsonValue)], key: &str) -> Option<&'a JsonValue> {
    fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Pulls `choices[0].message.content` out of an OpenAI-compatible
/// chat-completions response body. Any deviation from that shape - invalid
/// JSON, a missing/empty `choices`, a `message` with no string `content`,
/// or a top-level value that isn't even an object - becomes
/// `LlmError::Malformed` with a short reason, never a panic and never a
/// silent empty string standing in for "the server sent something odd".
fn extract_message_content(body: &str) -> Result<String, LlmError> {
    let value = parse_json(body).map_err(|_| LlmError::Malformed("response body is not valid JSON".to_string()))?;
    let JsonValue::Object(root) = value else {
        return Err(LlmError::Malformed("response body is not a JSON object".to_string()));
    };
    let choices = match object_get(&root, "choices") {
        Some(JsonValue::Array(items)) => items,
        _ => return Err(LlmError::Malformed("response has no \"choices\" array".to_string())),
    };
    let JsonValue::Object(first) = choices.first().ok_or_else(|| LlmError::Malformed("\"choices\" array is empty".to_string()))? else {
        return Err(LlmError::Malformed("choices[0] is not an object".to_string()));
    };
    let JsonValue::Object(message) = object_get(first, "message")
        .ok_or_else(|| LlmError::Malformed("choices[0] has no \"message\"".to_string()))?
    else {
        return Err(LlmError::Malformed("choices[0].message is not an object".to_string()));
    };
    match object_get(message, "content") {
        Some(JsonValue::String(content)) => Ok(content.clone()),
        _ => Err(LlmError::Malformed("choices[0].message has no string \"content\"".to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure string formatting - no socket opened, so this is safe to run
    /// anywhere `MockBackend`'s tests are (see this module's doc: nothing
    /// here ever touches the network in a test).
    ///
    /// `http_request_body_is_openai_compatible`: the shape Ollama, llama.cpp
    /// and vLLM all expect on `/v1/chat/completions` - `model`, then a
    /// `messages` array with exactly one `system` and one `user` message in
    /// that order, then `max_tokens`.
    #[test]
    fn http_request_body_is_openai_compatible() {
        let request = LlmRequest {
            system: "sys \"quoted\"\nline".to_string(),
            user: "user\ttext".to_string(),
            max_output_tokens: 400,
        };
        let body = build_request_json(&request, "llama3");
        assert_eq!(
            body,
            r#"{"model":"llama3","messages":[{"role":"system","content":"sys \"quoted\"\nline"},{"role":"user","content":"user\ttext"}],"max_tokens":400}"#
        );

        // And it must itself be valid JSON with the fields at the expected
        // paths, not just a string that happens to look right.
        let parsed = parse_json(&body).expect("built request body must be valid JSON");
        let JsonValue::Object(fields) = parsed else { panic!("expected object") };
        assert!(matches!(object_get(&fields, "model"), Some(JsonValue::String(m)) if m == "llama3"));
        let Some(JsonValue::Array(messages)) = object_get(&fields, "messages") else {
            panic!("expected \"messages\" array")
        };
        assert_eq!(messages.len(), 2);
        let JsonValue::Object(sys_msg) = &messages[0] else { panic!("expected object") };
        assert!(matches!(object_get(sys_msg, "role"), Some(JsonValue::String(r)) if r == "system"));
        let JsonValue::Object(user_msg) = &messages[1] else { panic!("expected object") };
        assert!(matches!(object_get(user_msg, "role"), Some(JsonValue::String(r)) if r == "user"));
        assert!(matches!(object_get(&fields, "max_tokens"), Some(JsonValue::Number(n)) if *n == 400.0));
    }

    #[test]
    fn splits_status_and_body() {
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\n{\"posture\":\"defensive\"}";
        let (status, body) = split_http_response(response).unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, "{\"posture\":\"defensive\"}");
    }

    #[test]
    fn rejects_response_with_no_body_separator() {
        assert!(split_http_response("not an http response").is_none());
    }

    /// `http_response_envelope_is_parsed`: a canned OpenAI-shaped response
    /// yields the assistant content, and a malformed envelope yields
    /// `LlmError::Malformed` rather than a panic or a silently empty string.
    #[test]
    fn http_response_envelope_is_parsed() {
        let response = r#"{"id":"x","choices":[{"index":0,"message":{"role":"assistant","content":"{\"posture\":\"defensive\"}"},"finish_reason":"stop"}],"usage":{"total_tokens":42}}"#;
        let content = extract_message_content(response).expect("well-shaped envelope must parse");
        assert_eq!(content, r#"{"posture":"defensive"}"#);
    }

    #[test]
    fn malformed_envelope_is_rejected() {
        let cases = [
            "not json at all",
            "[]",                                              // not an object
            r#"{"no_choices_here":true}"#,                      // missing "choices"
            r#"{"choices":[]}"#,                                // empty choices
            r#"{"choices":[{"no_message":true}]}"#,             // missing "message"
            r#"{"choices":[{"message":{"no_content":true}}]}"#, // missing "content"
            r#"{"choices":[{"message":{"content":42}}]}"#,      // content not a string
        ];
        for case in cases {
            assert!(
                matches!(extract_message_content(case), Err(LlmError::Malformed(_))),
                "expected {case:?} to be rejected as Malformed"
            );
        }
    }
}
