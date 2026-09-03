//! A minimal, std-only HTTP/1.1 request reader and response writer.
//!
//! Only what this API actually needs: a request line, headers, an optional
//! `Content-Length` body, and a small set of status codes. No chunked
//! transfer encoding, no keep-alive pipelining - every connection is
//! handled as exactly one request/response (or one WebSocket upgrade) and
//! then closed, which is both simpler and easier to reason about under
//! hostile input than a general-purpose server would need to be.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;

/// Hard cap on a request's `Content-Length` (and on however many bytes of
/// body this reads even if the header lies) - docs/phase5-spec.md
/// "リクエストサイズに上限を設ける". 1 MiB is generously above any
/// legitimate `/action` or `/reset` body this API defines.
pub const MAX_BODY_BYTES: usize = 1 << 20;

/// Cap on the request line + headers, independent of `MAX_BODY_BYTES` - a
/// client that never sends a blank line (or sends an enormous header
/// block) must not be able to make this thread buffer unbounded memory.
const MAX_HEADER_BYTES: usize = 16 * 1024;

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub enum ReadError {
    /// The connection closed, or otherwise sent nothing usable, before a
    /// full request line arrived - not necessarily a client error worth
    /// logging (a health-checker probing the port with no bytes at all
    /// looks exactly like this too).
    Empty,
    /// The request line/headers exceeded `MAX_HEADER_BYTES`, or a header
    /// line was malformed.
    Malformed(String),
    /// `Content-Length` exceeded `MAX_BODY_BYTES` - the caller should
    /// respond `413` and close the connection without reading the rest of
    /// the body.
    TooLarge,
    Io(std::io::Error),
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(query: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if query.is_empty() {
        return map;
    }
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let mut parts = pair.splitn(2, '=');
        let k = parts.next().unwrap_or("");
        let v = parts.next().unwrap_or("");
        map.insert(percent_decode(k), percent_decode(v));
    }
    map
}

/// Reads and parses one HTTP request from `stream`. `reader` is the
/// buffered wrapper the caller keeps around across this call (so the same
/// buffered bytes can later be handed to a WebSocket upgrade without loss).
pub fn read_request(reader: &mut BufReader<TcpStream>) -> Result<Request, ReadError> {
    let mut header_bytes = 0usize;
    let mut line = String::new();
    let n = reader.read_line(&mut line).map_err(ReadError::Io)?;
    if n == 0 {
        return Err(ReadError::Empty);
    }
    header_bytes += n;
    let line = line.trim_end_matches(['\r', '\n']);
    let mut parts = line.split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let raw_path = parts.next().unwrap_or("").to_string();
    let _version = parts.next().unwrap_or("");
    if method.is_empty() || raw_path.is_empty() {
        return Err(ReadError::Malformed("bad request line".to_string()));
    }

    let (path, query) = match raw_path.split_once('?') {
        Some((p, q)) => (percent_decode(p), parse_query(q)),
        None => (percent_decode(&raw_path), BTreeMap::new()),
    };

    let mut headers = BTreeMap::new();
    loop {
        let mut hline = String::new();
        let n = reader.read_line(&mut hline).map_err(ReadError::Io)?;
        if n == 0 {
            return Err(ReadError::Malformed("connection closed mid-headers".to_string()));
        }
        header_bytes += n;
        if header_bytes > MAX_HEADER_BYTES {
            return Err(ReadError::Malformed("headers too large".to_string()));
        }
        let hline = hline.trim_end_matches(['\r', '\n']);
        if hline.is_empty() {
            break;
        }
        if let Some((k, v)) = hline.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let body = if let Some(len_s) = headers.get("content-length") {
        let len: usize = len_s.parse().map_err(|_| ReadError::Malformed("bad Content-Length".to_string()))?;
        if len > MAX_BODY_BYTES {
            // Drain (and discard) whatever the client actually sends, up to
            // a generous bound, before the caller responds and closes the
            // connection. Skipping this and just closing works too, but a
            // socket closed while the kernel still has unread bytes queued
            // for it typically sends a TCP RST instead of a clean FIN - the
            // client then never reliably sees the `413` response this
            // produces, it just sees "connection reset". Draining first
            // means the response actually arrives even though the request
            // was refused. The bound (independent of whatever huge number
            // the client claimed) keeps a client that lies about
            // `Content-Length` and then sends nothing from tying up this
            // thread forever.
            let drain_cap = (MAX_BODY_BYTES * 8) as u64;
            let to_drain = (len as u64).min(drain_cap);
            let mut sink = std::io::sink();
            let _ = std::io::copy(&mut reader.by_ref().take(to_drain), &mut sink);
            return Err(ReadError::TooLarge);
        }
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).map_err(ReadError::Io)?;
        buf
    } else {
        Vec::new()
    };

    Ok(Request { method, path, query, headers, body })
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json(status: u16, body: &crate::json::Value) -> Response {
        Response {
            status,
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: body.to_json().into_bytes(),
        }
    }

    pub fn plain(status: u16, body: impl Into<String>) -> Response {
        Response {
            status,
            headers: vec![("Content-Type".to_string(), "text/plain; charset=utf-8".to_string())],
            body: body.into().into_bytes(),
        }
    }

    pub fn error(status: u16, reason: &str) -> Response {
        Response::json(status, &crate::json::Value::obj(vec![("error", crate::json::Value::str(reason))]))
    }
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        _ => "Error",
    }
}

pub fn write_response(stream: &mut TcpStream, resp: &Response) -> std::io::Result<()> {
    let mut out = format!("HTTP/1.1 {} {}\r\n", resp.status, status_text(resp.status));
    out.push_str(&format!("Content-Length: {}\r\n", resp.body.len()));
    for (k, v) in &resp.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("Connection: close\r\n\r\n");
    stream.write_all(out.as_bytes())?;
    stream.write_all(&resp.body)?;
    stream.flush()
}
