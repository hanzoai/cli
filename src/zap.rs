//! ZAP-HTTP — the native wire, spoken directly.
//!
//! ZAP is the fleet's primary transport; HTTP is a SECONDARY VIEW of the same
//! routes, kept for third parties who cannot speak it. The `hanzo` CLI is FIRST
//! PARTY, so against a local host it speaks the native wire rather than asking
//! the server to render an HTTP view we would then have to parse back. No
//! HTTP/1.1 text grammar, no chunked framing, no second contract to keep in
//! agreement with the first.
//!
//! What this does NOT change, said plainly: the response BODY is still cloud's
//! `/v1` JSON envelope. ZAP replaces the message framing and header encoding,
//! not the payload format. The win is one wire instead of two, and a decoder
//! whose header names and body are SUBSLICES of the received frame — nothing is
//! re-parsed or copied out to read them.
//!
//! # Wire (github.com/zap-proto/http v0.3.1, pinned by golden vectors)
//!
//! On the socket each message is `[u32 BE length][frame]` — big-endian, and the
//! ONLY big-endian quantity here; everything inside the frame is little-endian.
//!
//! A frame is a 16-byte header — magic `ZAP\0`, `version:u16=1`, `flags:u16`
//! whose high byte is the frame type, `rootOffset:u32=16`, `size:u32` = the
//! whole frame — followed by a 48-byte fixed root of six 8-byte slots, then a
//! variable tail. Each slot is `{relOffset:u32, length:u32}` where relOffset is
//! measured from THE SLOT'S OWN position; an empty field keeps a zeroed `{0,0}`
//! slot and contributes no tail bytes.
//!
//! Headers are not JSON and not a text block: a `u32` pair count, then
//! `{u32 keylen, key, u32 vallen, value}` per pair.
//!
//! There is no handshake and no encryption on this path — zaphttp's transport
//! is plaintext framing over the socket today (its own TODO reserves an X-Wing
//! PQ KEM handshake for later). A unix socket in a user-owned directory is the
//! confidentiality boundary; this client must never be pointed at a remote TCP
//! peer while that remains true, which is why only a local socket reaches here.

use anyhow::{anyhow, bail, Context, Result};
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

const MAGIC: &[u8; 4] = b"ZAP\0";
const HEADER: usize = 16;
const VERSION: u16 = 1;
/// The hardened runtime (luxfi/zap) stamps 2. Its data segment is identical for
/// these frames, and the reference decoder accepts both, so this client must too.
const VERSION_HARDENED: u16 = 2;

/// Frame type, carried in the header flags' high byte (`flags >> 8`).
const FRAME_REQUEST: u16 = 0x01;
const FRAME_RESPONSE: u16 = 0x02;
const FRAME_RESPONSE_HEAD: u16 = 0x03;

/// The root object's fixed section: six 8-byte slots, in declaration order.
const SLOTS: usize = 48;
const REQ_METHOD: usize = 0;
const REQ_TARGET: usize = 8;
const REQ_PROTO: usize = 16;
const REQ_HEADERS: usize = 24;
const REQ_BODY: usize = 32;
const RESP_STATUS: usize = 0;
const RESP_BODY: usize = 32;

/// The reference transport's ceiling. Matching it turns a corrupt or hostile
/// length prefix into a named error instead of a 4 GiB allocation.
const MAX_FRAME: u32 = 64 << 20;

/// Send one request over a local ZAP socket and return `(status, parsed body)`,
/// the same shape [`crate::http::send`] returns so the two transports are
/// interchangeable at the one call seam.
pub(crate) async fn send(
    sock: &Path,
    method: &Method,
    target: &str,
    token: &str,
    body: Option<&Value>,
) -> Result<(StatusCode, Value)> {
    let payload = match body {
        Some(v) => serde_json::to_vec(v).context("encoding the request body")?,
        None => Vec::new(),
    };

    // Host, Content-Length and Trailer are FRAME-OWNED — the reference encoder
    // skips them because the frame already carries that information, and sending
    // them would put two sources of truth on the wire.
    let mut headers: Vec<(&str, String)> = Vec::new();
    if !token.is_empty() {
        headers.push(("Authorization", format!("Bearer {token}")));
    }
    if !payload.is_empty() {
        headers.push(("Content-Type", "application/json".to_string()));
    }

    let frame = encode_request(method.as_str(), target, &headers, &payload);

    let mut conn = UnixStream::connect(sock)
        .await
        .with_context(|| format!("connecting to the local cloud host at {}", sock.display()))?;
    write_frame(&mut conn, &frame).await.context("sending the request frame")?;
    let reply = read_frame(&mut conn).await.context("reading the response frame")?;

    let (status, body) = decode_response(&reply)?;
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    if body.is_empty() {
        return Ok((status, Value::Null));
    }
    // A body that is not JSON is still the caller's to show — the same rule the
    // HTTP seam follows, so an error page never becomes a parse failure here.
    let value = serde_json::from_slice(body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).into_owned()));
    Ok((status, value))
}

// ---- framing ----------------------------------------------------------------

async fn write_frame(conn: &mut UnixStream, frame: &[u8]) -> Result<()> {
    // BIG endian, and only here: the length prefix is the transport's, the frame
    // body is the codec's, and they disagree on byte order by design.
    conn.write_all(&(frame.len() as u32).to_be_bytes()).await?;
    conn.write_all(frame).await?;
    conn.flush().await?;
    Ok(())
}

async fn read_frame(conn: &mut UnixStream) -> Result<Vec<u8>> {
    let mut prefix = [0u8; 4];
    conn.read_exact(&mut prefix).await?;
    let n = u32::from_be_bytes(prefix);
    if n == 0 {
        bail!("zero-length frame");
    }
    if n > MAX_FRAME {
        bail!("frame of {n} bytes exceeds the {MAX_FRAME}-byte maximum");
    }
    let mut buf = vec![0u8; n as usize];
    conn.read_exact(&mut buf).await?;
    Ok(buf)
}

// ---- codec ------------------------------------------------------------------

fn encode_request(method: &str, target: &str, headers: &[(&str, String)], body: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(HEADER + SLOTS + target.len() + body.len() + 64);
    f.extend_from_slice(MAGIC);
    f.extend_from_slice(&VERSION.to_le_bytes());
    f.extend_from_slice(&(FRAME_REQUEST << 8).to_le_bytes());
    f.extend_from_slice(&(HEADER as u32).to_le_bytes());
    f.extend_from_slice(&0u32.to_le_bytes()); // size — patched once the tail is known

    let obj = f.len();
    f.resize(obj + SLOTS, 0);

    put_var(&mut f, obj + REQ_METHOD, method.as_bytes());
    put_var(&mut f, obj + REQ_TARGET, target.as_bytes());
    // The reference encoder writes whatever the request reports, which is
    // HTTP/1.1 for one built the ordinary way. The golden vectors pin it.
    put_var(&mut f, obj + REQ_PROTO, b"HTTP/1.1");

    if !headers.is_empty() {
        let start = f.len();
        f.extend_from_slice(&(headers.len() as u32).to_le_bytes());
        for (k, v) in headers {
            f.extend_from_slice(&(k.len() as u32).to_le_bytes());
            f.extend_from_slice(k.as_bytes());
            f.extend_from_slice(&(v.len() as u32).to_le_bytes());
            f.extend_from_slice(v.as_bytes());
        }
        patch(&mut f, obj + REQ_HEADERS, start);
    }
    put_var(&mut f, obj + REQ_BODY, body);
    // The trailer slot stays {0,0}: a CLI request has no trailers.

    let size = f.len() as u32;
    f[12..16].copy_from_slice(&size.to_le_bytes());
    f
}

/// Status and body. Header names/values are left in the frame — the CLI reads
/// neither, and decoding them would be work done to throw away.
fn decode_response(f: &[u8]) -> Result<(u16, &[u8])> {
    if f.len() < HEADER {
        bail!("short frame: {} bytes", f.len());
    }
    if &f[0..4] != MAGIC {
        bail!("not a ZAP frame (bad magic)");
    }
    // 1 is this runtime's baseline; 2 is the hardened runtime (luxfi/zap), whose
    // data segment is identical for these frames. The reference decoder accepts
    // both, so refusing 2 here would reject a peer the Go side talks to happily.
    let version = u16::from_le_bytes([f[4], f[5]]);
    if version != VERSION && version != VERSION_HARDENED {
        bail!("ZAP wire version {version} is not one this client reads (1 or 2)");
    }
    match u16::from_le_bytes([f[6], f[7]]) >> 8 {
        FRAME_RESPONSE => {}
        // Named rather than silently truncated: a streamed body arrives as
        // head/data/end frames, which this client does not reassemble.
        FRAME_RESPONSE_HEAD => bail!("the host streamed this response; the ZAP client reads single-frame replies only"),
        other => bail!("expected a response frame, got type {other:#04x}"),
    }
    let root = u32::from_le_bytes([f[8], f[9], f[10], f[11]]) as usize;
    // `size` — not the buffer length — bounds every read below, exactly as the
    // reference decoder does: trailing bytes past `size` are not part of the frame.
    let size = u32::from_le_bytes([f[12], f[13], f[14], f[15]]) as usize;
    if size < HEADER || size > f.len() {
        bail!("frame declares {size} bytes but {} arrived", f.len());
    }
    if root < HEADER || root >= size {
        bail!("root offset {root} is outside the {size}-byte frame");
    }
    if root + SLOTS > size {
        bail!("root object at {root} does not fit in {size} bytes");
    }
    let status = u16::from_le_bytes([f[root + RESP_STATUS], f[root + RESP_STATUS + 1]]);
    let body = read_var(f, size, root + RESP_BODY)?;
    Ok((status, body))
}

/// Append `data` to the tail and point `slot` at it. An EMPTY field keeps its
/// zeroed `{0,0}` slot and contributes no tail bytes.
fn put_var(f: &mut Vec<u8>, slot: usize, data: &[u8]) {
    if data.is_empty() {
        return;
    }
    let start = f.len();
    f.extend_from_slice(data);
    patch(f, slot, start);
}

/// A slot is `{relOffset, length}`, both u32 LE, and relOffset is measured from
/// the SLOT's own position — not from the frame or the object.
fn patch(f: &mut [u8], slot: usize, start: usize) {
    let rel = (start - slot) as u32;
    let len = (f.len() - start) as u32;
    f[slot..slot + 4].copy_from_slice(&rel.to_le_bytes());
    f[slot + 4..slot + 8].copy_from_slice(&len.to_le_bytes());
}

/// Read one slot's bytes as a SUBSLICE of the frame — this is the zero-copy
/// property: nothing is allocated or copied to look at a field.
fn read_var(f: &[u8], size: usize, slot: usize) -> Result<&[u8]> {
    if slot + 8 > size {
        bail!("slot at {slot} runs past the {size}-byte frame");
    }
    let rel = u32::from_le_bytes([f[slot], f[slot + 1], f[slot + 2], f[slot + 3]]) as usize;
    // relOffset 0 is NULL — the length word is not consulted. Reading the length
    // first would let a `{0, n}` slot alias the slot's own bytes as a value.
    if rel == 0 {
        return Ok(&[]);
    }
    let len = u32::from_le_bytes([f[slot + 4], f[slot + 5], f[slot + 6], f[slot + 7]]) as usize;
    let start = slot.checked_add(rel).ok_or_else(|| anyhow!("slot offset overflow"))?;
    let end = start.checked_add(len).ok_or_else(|| anyhow!("slot length overflow"))?;
    if start < HEADER || end > size {
        bail!("field at {start}+{len} runs past the {size}-byte frame");
    }
    Ok(&f[start..end])
}

#[cfg(test)]
mod tests;
