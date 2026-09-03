//! A minimal RFC 6455 WebSocket server, just enough to serve `GET /watch`
//! (docs/phase5-spec.md "WS /watch ?session_id -> tick ごとの Event を配信")
//! with no external crate: a hand-rolled SHA-1 (for the handshake's
//! `Sec-WebSocket-Accept`), a hand-rolled base64 encoder, and a
//! server-to-client-only text-frame writer (this server only ever pushes
//! `Event` JSON out - it doesn't need to parse client-sent data frames,
//! only recognize a close frame so it can stop pushing to a dead
//! connection).

use std::io::{Read, Write};
use std::net::TcpStream;

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Computes `Sec-WebSocket-Accept` from the client's `Sec-WebSocket-Key`
/// per RFC 6455 §1.3: base64(SHA-1(key + GUID)).
pub fn accept_key(client_key: &str) -> String {
    let mut input = client_key.as_bytes().to_vec();
    input.extend_from_slice(WS_GUID.as_bytes());
    let digest = sha1(&input);
    base64_encode(&digest)
}

/// Writes the `101 Switching Protocols` handshake response.
pub fn write_handshake(stream: &mut TcpStream, client_key: &str) -> std::io::Result<()> {
    let accept = accept_key(client_key);
    let resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    stream.write_all(resp.as_bytes())
}

/// Writes one unmasked text frame (server->client frames are never masked
/// per RFC 6455 §5.1) carrying `payload`. Fragments nothing - every event
/// this server sends is small enough for a single frame.
pub fn write_text_frame(stream: &mut TcpStream, payload: &str) -> std::io::Result<()> {
    let bytes = payload.as_bytes();
    let mut frame = Vec::with_capacity(bytes.len() + 10);
    frame.push(0x81); // FIN=1, opcode=1 (text)
    let len = bytes.len();
    if len <= 125 {
        frame.push(len as u8);
    } else if len <= 0xFFFF {
        frame.push(126);
        frame.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        frame.push(127);
        frame.extend_from_slice(&(len as u64).to_be_bytes());
    }
    frame.extend_from_slice(bytes);
    stream.write_all(&frame)
}

/// Writes a close frame (opcode 8, empty payload).
pub fn write_close_frame(stream: &mut TcpStream) -> std::io::Result<()> {
    stream.write_all(&[0x88, 0x00])
}

/// Reads one client-sent frame's opcode, just enough to notice a close (8)
/// or a ping (9) - text/binary payloads from the client are read and
/// discarded (`/watch` is a one-way event stream; this only exists so a
/// polite client close, or an idle keepalive ping, doesn't look like a
/// broken pipe). Returns `None` on any I/O error or malformed frame -
/// treated by the caller as "stop streaming to this connection", never a
/// panic.
pub fn read_frame_opcode(stream: &mut TcpStream) -> Option<u8> {
    let mut header = [0u8; 2];
    stream.read_exact(&mut header).ok()?;
    let opcode = header[0] & 0x0F;
    let masked = header[1] & 0x80 != 0;
    let mut len = (header[1] & 0x7F) as u64;
    if len == 126 {
        let mut ext = [0u8; 2];
        stream.read_exact(&mut ext).ok()?;
        len = u16::from_be_bytes(ext) as u64;
    } else if len == 127 {
        let mut ext = [0u8; 8];
        stream.read_exact(&mut ext).ok()?;
        len = u64::from_be_bytes(ext);
    }
    // Bound how much a client can make us read for one frame - this
    // connection only ever needs to notice control frames, never a large
    // payload.
    if len > (1 << 20) {
        return None;
    }
    let mask = if masked {
        let mut m = [0u8; 4];
        stream.read_exact(&mut m).ok()?;
        Some(m)
    } else {
        None
    };
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).ok()?;
    let _ = mask; // payload contents are discarded either way
    Some(opcode)
}

const BASE64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied();
        let b2 = chunk.get(2).copied();
        out.push(BASE64_CHARS[(b0 >> 2) as usize] as char);
        out.push(BASE64_CHARS[(((b0 & 0x03) << 4) | (b1.unwrap_or(0) >> 4)) as usize] as char);
        if let Some(b1) = b1 {
            out.push(BASE64_CHARS[(((b1 & 0x0F) << 2) | (b2.unwrap_or(0) >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if let Some(b2) = b2 {
            out.push(BASE64_CHARS[(b2 & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// SHA-1 (FIPS 180-4). Not used for anything security-sensitive - the
/// WebSocket handshake uses it only as the specific hash RFC 6455 mandates
/// for `Sec-WebSocket-Accept`, a protocol handshake formality, not an
/// authentication mechanism.
fn sha1(message: &[u8]) -> [u8; 20] {
    let mut h0: u32 = 0x67452301;
    let mut h1: u32 = 0xEFCDAB89;
    let mut h2: u32 = 0x98BADCFE;
    let mut h3: u32 = 0x10325476;
    let mut h4: u32 = 0xC3D2E1F0;

    let ml = (message.len() as u64) * 8;
    let mut padded = message.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&ml.to_be_bytes());

    for chunk in padded.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) = (h0, h1, h2, h3, h4);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = if i < 20 {
                ((b & c) | ((!b) & d), 0x5A827999u32)
            } else if i < 40 {
                (b ^ c ^ d, 0x6ED9EBA1u32)
            } else if i < 60 {
                ((b & c) | (b & d) | (c & d), 0x8F1BBCDCu32)
            } else {
                (b ^ c ^ d, 0xCA62C1D6u32)
            };
            let temp = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut out = [0u8; 20];
    out[0..4].copy_from_slice(&h0.to_be_bytes());
    out[4..8].copy_from_slice(&h1.to_be_bytes());
    out[8..12].copy_from_slice(&h2.to_be_bytes());
    out[12..16].copy_from_slice(&h3.to_be_bytes());
    out[16..20].copy_from_slice(&h4.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6455 §1.3's own worked example.
    #[test]
    fn accept_key_matches_rfc_example() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn sha1_matches_known_vectors() {
        assert_eq!(
            base64_encode(&sha1(b"")),
            base64_encode(&hex_decode("da39a3ee5e6b4b0d3255bfef95601890afd80709")),
        );
        assert_eq!(
            base64_encode(&sha1(b"abc")),
            base64_encode(&hex_decode("a9993e364706816aba3e25717850c26c9cd0d89d")),
        );
    }

    fn hex_decode(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }
}
