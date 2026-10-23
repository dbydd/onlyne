//! `onlyne-wire` — the wire seam: length-prefixed JSON frames, and the local
//! socket one daemon binds and every other process reaches.
//!
//! [`frame`] moves opaque serialisable values and holds no protocol semantics,
//! so `onlyne-proto` stays free of tokio. [`socket`] owns the owner-tree
//! endpoint: the machine-level runtime directory (`/tmp/onlyne-<uid>/`), the
//! `<digest>.sock` a daemon binds there, the `<digest>.json` registration that
//! names the surface serving it, and the owner-only modes around both.
//!
//! The frame API is re-exported at the crate root, because a caller that wants
//! frames wants nothing else from this crate; the socket API stays behind its
//! module, since `SocketEndpoint` and `bind_socket` read better named.

pub mod frame;
pub mod socket;

pub use frame::{
    FrameReader, MAX_FRAME_BYTES, MAX_PARTIAL_FRAME_BYTES, TooLarge, UnexpectedEof, is_bad_frame,
    is_too_large, read_frame, write_frame,
};
