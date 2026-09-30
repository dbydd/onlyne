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
//!
//! The decoder never buffers more than one frame body at a time, and the ceiling
//! is applied before any body byte is read: the announced length is checked
//! against [`MAX_FRAME_BYTES`], and [`fill`] refuses a destination larger than
//! [`MAX_PARTIAL_FRAME_BYTES`]. A sender that dribbles bytes therefore holds the
//! reader's memory to the announced length it was allowed to announce, so
//! `MAX_FRAME_BYTES` is the binding limit and `MAX_PARTIAL_FRAME_BYTES` is the
//! outer wall behind it.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt;
use std::io::{Error, ErrorKind, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Hard ceiling for one frame body. Larger payloads are rejected before any byte
/// reaches the stream, which keeps the connection framing consistent.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Outer ceiling for the bytes one decode holds: the body buffer, on top of the
/// four header bytes that name it.
///
/// [`MAX_FRAME_BYTES`] is the limit a peer actually meets: an announced length
/// over it fails before the body is read. This ceiling sits behind that check and
/// is enforced in [`fill`], the one place stream bytes land in memory, so the
/// accumulation bound stays true even if the per-frame ceiling is ever raised or a
/// new caller hands `fill` a buffer sized from untrusted input. Reading a frame
/// cannot buffer past this many bytes, however slowly the sender dribbles them.
pub const MAX_PARTIAL_FRAME_BYTES: usize = 16 * 1024 * 1024;

// The per-frame gate must stay inside the accumulation wall, or the tighter check
// stops being the one a peer meets.
const _: () = assert!(MAX_FRAME_BYTES + 4 <= MAX_PARTIAL_FRAME_BYTES);

/// A frame ran past a ceiling: [`MAX_FRAME_BYTES`] for one body, announced or
/// serialised, or [`MAX_PARTIAL_FRAME_BYTES`] when the decoder's own accumulation
/// bound is what a read ran into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooLarge {
    /// Length the peer announced, or the length the local side serialised to.
    pub len: u64,
    /// The ceiling that was crossed: [`MAX_FRAME_BYTES`] for a frame body, or
    /// [`MAX_PARTIAL_FRAME_BYTES`] when the decoder's own accumulation bound is
    /// what a read ran into.
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

fn too_large(len: u64, max: usize) -> Error {
    Error::new(ErrorKind::InvalidInput, TooLarge { len, max })
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
        return Err(too_large(len as u64, MAX_FRAME_BYTES));
    }
    let prefix = u32::try_from(len).map_err(|_| too_large(len as u64, MAX_FRAME_BYTES))?;
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
        return Err(too_large(len, MAX_FRAME_BYTES));
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
///
/// `dst` must fit the accumulation ceiling: a destination larger than
/// [`MAX_PARTIAL_FRAME_BYTES`] is refused with [`TooLarge`] before a byte is read,
/// which is what caps the memory one decode holds, whatever the peer announces and
/// however slowly it sends them.
async fn fill<R>(r: &mut R, dst: &mut [u8], announced: u64) -> Result<bool>
where
    R: AsyncRead + Unpin,
{
    if dst.len() > MAX_PARTIAL_FRAME_BYTES {
        return Err(too_large(dst.len() as u64, MAX_PARTIAL_FRAME_BYTES));
    }
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

/// Bytes one [`FrameReader::next`] asks the stream for before it knows a length.
const READ_CHUNK: usize = 64 * 1024;

/// A frame decoder whose partial progress outlives the future that reads it.
///
/// [`read_frame`] keeps the bytes it has read inside its own future, so a
/// caller that drops the future mid-frame — `tokio::select!` choosing another
/// branch — loses them and every later frame boundary with them. This reader
/// keeps them in `buf` instead: [`FrameReader::next`] awaits only
/// [`AsyncReadExt::read_buf`], which is cancel-safe, so dropping a `next`
/// future discards nothing, and the next call resumes where the stream is.
#[derive(Debug, Default)]
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read one frame. Cancel-safe: see the type's documentation.
    ///
    /// `Ok(None)` means the peer closed the stream at a frame boundary.
    pub async fn next<R, T>(&mut self, r: &mut R) -> Result<Option<T>>
    where
        R: AsyncRead + Unpin,
        T: DeserializeOwned,
    {
        loop {
            if let Some(end) = self.frame_end()? {
                let decoded = serde_json::from_slice::<T>(&self.buf[4..end]);
                self.buf.drain(..end);
                return decoded.map(Some).map_err(|e| {
                    Error::new(ErrorKind::InvalidData, format!("bad frame json: {e}"))
                });
            }
            self.buf.reserve(READ_CHUNK);
            if r.read_buf(&mut self.buf).await? == 0 {
                if self.buf.is_empty() {
                    return Ok(None);
                }
                // Same accounting as `read_frame`: a short header counts against
                // its 4 bytes, a short body against the announced length.
                let got = self.buf.len() as u64;
                let eof = match self.announced() {
                    Some(len) => UnexpectedEof {
                        expected: len,
                        got: got - 4,
                    },
                    None => UnexpectedEof { expected: 4, got },
                };
                return Err(Error::new(ErrorKind::UnexpectedEof, eof));
            }
        }
    }

    fn announced(&self) -> Option<u64> {
        let header: [u8; 4] = self.buf.get(..4)?.try_into().ok()?;
        Some(u32::from_be_bytes(header) as u64)
    }

    /// The end offset of a complete frame at the front of the buffer, refusing
    /// an oversize length before any of its body is read.
    fn frame_end(&self) -> Result<Option<usize>> {
        let Some(len) = self.announced() else {
            return Ok(None);
        };
        if len > MAX_FRAME_BYTES as u64 {
            return Err(too_large(len, MAX_FRAME_BYTES));
        }
        let end = 4 + len as usize;
        Ok((self.buf.len() >= end).then_some(end))
    }
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
