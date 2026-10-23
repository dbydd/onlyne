//! Session hosting layer: the slot table, adapter transport binding, delivery,
//! settlement, and supervision clocks for one session.

pub mod accept;
pub mod adapter_socket;
pub mod claim;
pub mod dispatch;
pub mod handoff;
pub mod slice;
pub mod stall;
