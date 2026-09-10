//! Length-prefixed JSON framing for Onlyne v1 (decision D8).
//!
//! A frame is a `u32 big-endian byte length` followed by that many bytes of
//! UTF-8 JSON. One connection carries request, response, and event frames side
//! by side. This crate moves opaque serialisable values and holds no protocol
//! semantics, so `onlyne-proto` stays free of tokio.
//!
//! Failure modes are attached to an [`io::Error`] kind so callers can map them
//! onto a wire error code:
//!
//! - [`TooLarge`] -> `frame_too_large`
//! - undecodable JSON ([`io::ErrorKind::InvalidData`]) -> `bad_frame`
//! - [`UnexpectedEof`] -> the peer dropped the connection mid-frame
//! - a clean end of stream at a frame boundary -> [`read_frame`] yields `Ok(None)`

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt;
use std::io::{Error, ErrorKind, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Hard ceiling for one frame body. Larger payloads are rejected before any byte
/// reaches the stream, which keeps the connection framing consistent.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// A frame body crossed [`MAX_FRAME_BYTES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooLarge {
    /// Length the peer announced, or the length the local side serialised to.
    pub len: u64,
    /// The ceiling that was crossed.
    pub max: usize,
}

impl fmt::Display for TooLarge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "frame of {} bytes exceeds {max}",
            self.len,
            max = self.max
        )
    }
}

impl std::error::Error for TooLarge {}

/// The stream ended in the middle of a frame: fewer bytes arrived than the
/// length prefix announced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnexpectedEof {
    /// Bytes the length prefix promised.
    pub expected: u64,
    /// Bytes that actually arrived.
    pub got: u64,
}

impl fmt::Display for UnexpectedEof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "stream ended mid-frame: {} of {} bytes arrived",
            self.got, self.expected
        )
    }
}

impl std::error::Error for UnexpectedEof {}

fn too_large(len: u64) -> Error {
    Error::new(
        ErrorKind::InvalidInput,
        TooLarge {
            len,
            max: MAX_FRAME_BYTES,
        },
    )
}

/// Serialise `value`, prepend its length, then flush the pair as one frame.
///
/// The body is buffered in full first, so an oversize payload fails without
/// putting a partial frame on the wire.
pub async fn write_frame<W, T>(w: &mut W, value: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize + ?Sized,
{
    let body = serde_json::to_vec(value).map_err(Error::other)?;
    let len = body.len();
    if len > MAX_FRAME_BYTES {
        return Err(too_large(len as u64));
    }
    let prefix = u32::try_from(len).map_err(|_| too_large(len as u64))?;
    let mut buf = Vec::with_capacity(4 + len);
    buf.extend_from_slice(&prefix.to_be_bytes());
    buf.extend_from_slice(&body);
    w.write_all(&buf).await?;
    w.flush().await
}

/// Read one frame.
///
/// `Ok(None)` means the peer closed the stream while the reader sat at a frame
/// boundary.
pub async fn read_frame<R, T>(r: &mut R) -> Result<Option<T>>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut header = [0u8; 4];
    if !fill(r, &mut header, 4).await? {
        return Ok(None);
    }
    let len = u32::from_be_bytes(header) as u64;
    if len > MAX_FRAME_BYTES as u64 {
        return Err(too_large(len));
    }
    let mut body = vec![0u8; len as usize];
    if !body.is_empty() && !fill(r, &mut body, len).await? {
        return Err(Error::new(
            ErrorKind::UnexpectedEof,
            UnexpectedEof {
                expected: len,
                got: 0,
            },
        ));
    }
    serde_json::from_slice::<T>(&body)
        .map(Some)
        .map_err(|e| Error::new(ErrorKind::InvalidData, format!("bad frame json: {e}")))
}

/// Read until `dst` is full.
///
/// `Ok(true)` means the buffer filled. `Ok(false)` means the stream ended with
/// nothing read at all, which is a clean close at a frame boundary. Ending
/// partway through yields [`UnexpectedEof`] against `announced`, the total byte
/// count the caller promised for this read.
async fn fill<R>(r: &mut R, dst: &mut [u8], announced: u64) -> Result<bool>
where
    R: AsyncRead + Unpin,
{
    let mut got = 0usize;
    while got < dst.len() {
        match r.read(&mut dst[got..]).await {
            Ok(0) => {
                if got == 0 {
                    return Ok(false);
                }
                return Err(Error::new(
                    ErrorKind::UnexpectedEof,
                    UnexpectedEof {
                        expected: announced,
                        got: got as u64,
                    },
                ));
            }
            Ok(n) => got += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// `true` when `err` reports a frame over [`MAX_FRAME_BYTES`].
pub fn is_too_large(err: &Error) -> bool {
    err.get_ref()
        .and_then(|r| r.downcast_ref::<TooLarge>())
        .is_some()
}

/// `true` when `err` reports undecodable frame content.
pub fn is_bad_frame(err: &Error) -> bool {
    err.kind() == ErrorKind::InvalidData
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use tokio::io::duplex;

    fn frame_bytes(value: &Value) -> Vec<u8> {
        let body = serde_json::to_vec(value).expect("json");
        let mut buf = (body.len() as u32).to_be_bytes().to_vec();
        buf.extend_from_slice(&body);
        buf
    }

    #[tokio::test]
    async fn round_trips_a_frame() {
        let (mut a, mut b) = duplex(4096);
        let out = json!({"f": "ping", "t": 7});
        write_frame(&mut a, &out).await.expect("write");
        let got: Value = read_frame(&mut b).await.expect("read").expect("frame");
        assert_eq!(got, out);
    }

    #[tokio::test]
    async fn reassembles_a_split_frame() {
        let (mut a, mut b) = duplex(64);
        let body = json!({"f": "req", "id": "r1", "op": "pull", "args": {}});
        let bytes = frame_bytes(&body);
        let mut offset = 0;
        for step in [1usize, 2, 3, 4, 5] {
            a.write_all(&bytes[offset..offset + step])
                .await
                .expect("partial");
            offset += step;
        }
        a.write_all(&bytes[offset..]).await.expect("rest");
        let got: Value = read_frame(&mut b).await.expect("read").expect("frame");
        assert_eq!(got, body);
    }

    #[tokio::test]
    async fn reads_back_to_back_frames_from_one_stream() {
        let (mut a, mut b) = duplex(4096);
        let one = json!({"f": "pong", "t": 1, "server_seq": 2});
        let two = json!({"f": "ack", "seq": 41});
        write_frame(&mut a, &one).await.expect("write one");
        write_frame(&mut a, &two).await.expect("write two");
        drop(a);
        let g1: Value = read_frame(&mut b).await.expect("r1").expect("f1");
        let g2: Value = read_frame(&mut b).await.expect("r2").expect("f2");
        let g3: Option<Value> = read_frame(&mut b).await.expect("r3");
        assert_eq!((g1, g2, g3), (one, two, None));
    }
    #[tokio::test]
    async fn returns_none_on_clean_close_at_frame_boundary() {
        let (_a, mut b) = duplex(4096);
        drop(_a);
        let got: Option<Value> = read_frame(&mut b).await.expect("clean close reads as none");
        assert_eq!(got, None);
    }


    #[tokio::test]
    async fn rejects_an_oversize_announced_length() {
        let (mut a, mut b) = duplex(64);
        a.write_all(&(MAX_FRAME_BYTES as u32 + 1).to_be_bytes())
            .await
            .expect("header");
        let err = read_frame::<_, Value>(&mut b).await.expect_err("must fail");
        assert!(is_too_large(&err), "err = {err}");
    }

    #[tokio::test]
    async fn rejects_an_oversize_outgoing_body_without_writing() {
        let (mut a, mut peek) = duplex(16);
        let big = json!({"text": "x".repeat(MAX_FRAME_BYTES + 8)});
        let err = write_frame(&mut a, &big).await.expect_err("must fail");
        assert!(is_too_large(&err), "err = {err}");
        drop(a);
        let mut sink = Vec::new();
        peek.read_to_end(&mut sink).await.expect("read side");
        assert!(sink.is_empty(), "a rejected frame writes no bytes");
    }

    #[tokio::test]
    async fn reports_a_truncated_body_as_unexpected_eof() {
        let (mut a, mut b) = duplex(64);
        a.write_all(&20u32.to_be_bytes()).await.expect("header");
        a.write_all(b"short").await.expect("partial body");
        drop(a);
        let err = read_frame::<_, Value>(&mut b).await.expect_err("must fail");
        assert_eq!(err.kind(), ErrorKind::UnexpectedEof);
        let eof = err
            .get_ref()
            .and_then(|r| r.downcast_ref::<UnexpectedEof>())
            .expect("UnexpectedEof payload");
        assert_eq!(
            *eof,
            UnexpectedEof {
                expected: 20,
                got: 5
            }
        );
    }

    #[tokio::test]
    async fn reports_a_truncated_header_as_unexpected_eof() {
        let (mut a, mut b) = duplex(64);
        a.write_all(&[0, 0]).await.expect("two header bytes");
        drop(a);
        let err = read_frame::<_, Value>(&mut b).await.expect_err("must fail");
        assert_eq!(
            err.get_ref()
                .and_then(|r| r.downcast_ref::<UnexpectedEof>())
                .map(|e| e.got),
            Some(2)
        );
    }

    #[tokio::test]
    async fn reports_undecodable_json_as_bad_frame() {
        let (mut a, mut b) = duplex(64);
        a.write_all(&9u32.to_be_bytes()).await.expect("header");
        a.write_all(b"{not json").await.expect("body");
        let err = read_frame::<_, Value>(&mut b).await.expect_err("must fail");
        assert!(is_bad_frame(&err), "err = {err}");
    }
}
