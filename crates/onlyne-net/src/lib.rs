#![forbid(unsafe_code)]

mod error;
pub use error::NetError;

pub mod acl;
pub mod backoff;
pub mod conn;
pub mod handshake;
pub mod identity;
pub mod tls;

pub use acl::{
    AclDeny, AclDenyReason, AclEdge, AclTable, MsgClass, RoleAcl, acl_allows, table_from,
};
pub use backoff::Backoff;
pub use conn::{
    CLOSE_REASON, ClientConn, ConnHandle, ConnReadiness, ConnSettings, DEFAULT_RESYNC_LAG,
    GatewayConn, OUTBOUND_QUEUE_DEPTH, RESYNC_LAG_KIND, TcpListen, TlsConn, accept_tls, dial,
    resync_lag_of, retry_of,
};
pub use handshake::{
    Challenge, HandshakeOk, HelloAck, accept, accept_with_timeout, connect, connect_with_timeout,
};
pub use identity::{KEY_PREFIX, KeyPair, challenge_message, parse_public};
pub use tls::{
    ServerCert, client_config, gen_self_signed, load_or_create, server_config, spki_pin_of,
};
pub use tokio::net::TcpStream;
