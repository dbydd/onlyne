#![forbid(unsafe_code)]

mod error;
pub use error::NetError;

pub mod acl;
pub mod backoff;
pub mod conn;
pub mod handshake;
pub mod identity;
pub mod tls;

pub use acl::{acl_allows, table_from, AclDeny, AclDenyReason, AclTable, MsgClass, RoleAcl};
pub use backoff::Backoff;
pub use conn::{TcpListen, TlsConn};
pub use handshake::{accept, accept_with_timeout, offer, offer_with_timeout, Challenge, HandshakeOk, HelloAck};
pub use identity::{challenge_message, parse_public, KeyPair};
pub use tls::{client_config, gen_self_signed, load_or_create, server_config, spki_pin_of, ServerCert};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::tempdir;
    use tokio::io::duplex;

    #[test]
    fn backoff_sequence_and_reset() {
        let mut backoff = Backoff::new();
        let actual: Vec<_> = (0..9).map(|_| backoff.next().as_secs()).collect();
        assert_eq!(actual, vec![1, 2, 4, 8, 16, 32, 60, 60, 60]);
        assert_eq!(backoff.attempt(), 9);
        backoff.reset();
        assert_eq!(backoff.next(), Duration::from_secs(1));
        assert_eq!(backoff.with_jitter(0.0), Duration::from_millis(1000));
    }

    #[test]
    fn acl_matrix() {
        let sender = KeyPair::from_seed([1; 32]);
        let target = KeyPair::from_seed([2; 32]);
        let other = KeyPair::from_seed([3; 32]);
        let table = table_from([
            ("sender".to_string(), sender.public_str(), false, vec!["*".to_string()], vec!["target".to_string()]),
            ("target".to_string(), target.public_str(), false, vec!["sender".to_string()], vec![]),
            ("admin".to_string(), other.public_str(), true, vec!["*".to_string()], vec!["*".to_string()]),
        ]).unwrap();
        let deny = acl_allows(&table, "admin", "target", MsgClass::Task, None).unwrap_err();
        assert_eq!(deny.reason, AclDenyReason::SenderNotAllowed);
        assert_eq!(deny.field, "from.role");
        let deny = acl_allows(&table, "sender", "admin", MsgClass::Task, None).unwrap_err();
        assert_eq!(deny.reason, AclDenyReason::TargetNotAllowed);
        assert_eq!(deny.field, "to.role");
        let deny = acl_allows(&table, "sender", "missing", MsgClass::Task, None).unwrap_err();
        assert_eq!(deny.reason, AclDenyReason::UnknownRole);
        assert_eq!(deny.field, "to.role");
    }

    #[test]
    fn cert_pinning_persists() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("server.pem");
        let first = load_or_create(&path, "127.0.0.1").unwrap();
        let second = load_or_create(&path, "127.0.0.1").unwrap();
        assert_eq!(first.spki_pin, second.spki_pin);
        assert_eq!(spki_pin_of(&rustls_pemfile::certs(&mut std::io::BufReader::new(second.cert_pem.as_slice())).next().unwrap().unwrap()).unwrap(), second.spki_pin);
    }

    #[tokio::test]
    async fn handshake_round_trip_and_acl_refusal() {
        let key = KeyPair::from_seed([4; 32]);
        let table = table_from([("worker".to_string(), key.public_str(), false, vec!["*".to_string()], vec!["*".to_string()])]).unwrap();
        let (mut left, mut right) = duplex(16 * 1024);
        let server = tokio::spawn(async move { accept(&mut left, &table, 1).await });
        let ack = offer(&mut right, "worker", &key, 1, "agent", "1.0", false).await.unwrap();
        assert!(ack.ok);
        assert_eq!(server.await.unwrap().unwrap().role, "worker");
    }

    #[tokio::test]
    async fn tls_loopback_round_trip() {
        let cert = gen_self_signed("127.0.0.1", 1).unwrap();
        let config = server_config(&cert).unwrap();
        let mut listener = TcpListen::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let pin = cert.spki_pin.clone();
        let client = tokio::spawn(async move {
            let mut stream = TlsConn::connect(&address.to_string(), &pin).await.unwrap();
            stream.send_frame(&serde_json::json!({"side":"client"})).await.unwrap();
            stream.recv_frame::<serde_json::Value>().await.unwrap().unwrap()
        });
        let mut server = listener.accept_next(&config).await.unwrap();
        let request = server.recv_frame::<serde_json::Value>().await.unwrap().unwrap();
        assert_eq!(request["side"], "client");
        server.send_frame(&serde_json::json!({"side":"server"})).await.unwrap();
        assert_eq!(client.await.unwrap()["side"], "server");
    }
}
