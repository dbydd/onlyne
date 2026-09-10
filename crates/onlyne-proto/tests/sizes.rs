//! Size ceilings for the hot enums.
//!
//! Run `cargo run -p onlyne-proto --bin sizes` for the full table. Each ceiling
//! is the measured size rounded up to a power of two, so adding a large arm to a
//! hot enum fails here instead of landing quietly.

use onlyne_proto::{AdminOp, ClientOp, Frame, GatewayOp, HostOp, PluginOp};
use std::mem::size_of;

/// `(type, measured bytes, ceiling)`. The ceiling is the measurement rounded up
/// to a power of two.
const CEILINGS: [(&str, usize, usize); 6] = [
    ("ClientOp", size_of::<ClientOp>(), 256),
    ("AdminOp", size_of::<AdminOp>(), 256),
    ("GatewayOp", size_of::<GatewayOp>(), 128),
    ("PluginOp", size_of::<PluginOp>(), 256),
    ("HostOp", size_of::<HostOp>(), 256),
    ("Frame<ClientOp>", size_of::<Frame<ClientOp>>(), 256),
];

#[test]
fn hot_enums_stay_under_their_size_ceilings() {
    for (name, measured, ceiling) in CEILINGS {
        assert!(
            measured <= ceiling,
            "{name} grew to {measured} bytes, past its {ceiling}-byte ceiling; \
             box the dominant arm or raise the ceiling deliberately"
        );
    }
}
