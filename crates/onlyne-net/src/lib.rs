#![forbid(unsafe_code)]

mod error;
pub use error::NetError;

pub mod acl;
pub mod backoff;
pub mod conn;
pub mod handshake;
pub mod identity;
pub mod tls;

pub use acl::{AclDeny, AclDenyReason, AclTable, MsgClass, RoleAcl, acl_allows, table_from};
pub use backoff::Backoff;
pub use conn::{TcpListen, TlsConn};
pub use handshake::{
    Challenge, HandshakeOk, HelloAck, accept, accept_with_timeout, offer, offer_with_timeout,
};
pub use identity::{KEY_PREFIX, KeyPair, challenge_message, parse_public};
pub use tls::{
    ServerCert, client_config, gen_self_signed, load_or_create, server_config, spki_pin_of,
};

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
        assert_eq!(backoff.with_jitter(1.0), Duration::from_secs(2));
    }

    #[test]
    fn acl_matrix() {
        let sender = KeyPair::from_seed([1; 32]);
        let target = KeyPair::from_seed([2; 32]);
        let other = KeyPair::from_seed([3; 32]);
        let table = table_from([
            (
                "sender".to_string(),
                sender.public_str(),
                false,
                vec!["*".to_string()],
                vec!["target".to_string()],
            ),
            (
                "target".to_string(),
                target.public_str(),
                false,
                vec!["sender".to_string()],
                vec![],
            ),
            (
                "admin".to_string(),
                other.public_str(),
                true,
                vec!["*".to_string()],
                vec!["*".to_string()],
            ),
        ])
        .unwrap();
        let deny = acl_allows(&table, "admin", "target", MsgClass::Task, None).unwrap_err();
        assert_eq!(deny.reason, AclDenyReason::SenderNotAllowed);
        assert_eq!(deny.field, "from.role");
        let deny = acl_allows(&table, "sender", "admin", MsgClass::Task, None).unwrap_err();
        assert_eq!(deny.reason, AclDenyReason::TargetNotAllowed);
        assert_eq!(deny.field, "to.role");
        let deny = acl_allows(&table, "sender", "missing", MsgClass::Task, None).unwrap_err();
        assert_eq!(deny.reason, AclDenyReason::UnknownRole);
        assert_eq!(deny.field, "to.role");
        let allowed = acl_allows(&table, "admin", "admin", MsgClass::Control, None);
        assert!(allowed.is_ok());
        let allowed = acl_allows(
            &table,
            "sender",
            "target",
            MsgClass::Control,
            Some("sender"),
        );
        assert!(allowed.is_ok());
        let deny =
            acl_allows(&table, "sender", "target", MsgClass::Control, Some("admin")).unwrap_err();
        assert_eq!(deny.reason, AclDenyReason::AdminRequired);
        assert_eq!(deny.field, "admin");
    }

    #[test]
    fn cert_pinning_persists() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("server.pem");
        let first = load_or_create(&path, "127.0.0.1").unwrap();
        let second = load_or_create(&path, "127.0.0.1").unwrap();
        assert_eq!(first.spki_pin, second.spki_pin);
        assert_eq!(
            spki_pin_of(
                &rustls_pemfile::certs(&mut std::io::BufReader::new(second.cert_pem.as_slice()))
                    .next()
                    .unwrap()
                    .unwrap()
            )
            .unwrap(),
            second.spki_pin
        );
    }

    #[tokio::test]
    async fn handshake_round_trip_and_acl_refusal() {
        let key = KeyPair::from_seed([4; 32]);
        let table = table_from([(
            "worker".to_string(),
            key.public_str(),
            false,
            vec!["*".to_string()],
            vec!["*".to_string()],
        )])
        .unwrap();
        let (mut left, mut right) = duplex(16 * 1024);
        let server = tokio::spawn(async move { accept(&mut left, &table, 1).await });
        let ack = offer(&mut right, "worker", &key, 1, "agent", "1.0", false)
            .await
            .unwrap();
        assert!(ack.ok);
        assert_eq!(server.await.unwrap().unwrap().role, "worker");
    }

    #[tokio::test]
    async fn handshake_rejects_unexpected_events() {
        let registered = KeyPair::from_seed([11; 32]);
        let foreign = KeyPair::from_seed([12; 32]);
        let tamper_key = KeyPair::from_seed([13; 32]);
        let table = table_from([(
            "worker".to_string(),
            registered.public_str(),
            false,
            vec!["*".to_string()],
            vec!["*".to_string()],
        )])
        .unwrap();
        let timeout_limit = Duration::from_secs(5);

        let (mut left, mut right) = duplex(16 * 1024);
        let server_table = table.clone();
        let server = tokio::spawn(async move { accept(&mut left, &server_table, 1).await });
        let error = offer(&mut right, "worker", &foreign, 1, "agent", "1.0", false)
            .await
            .unwrap_err();
        assert!(matches!(error, NetError::Rejected { code, .. } if code == "unauthorized"));
        let server_error = server.await.unwrap().unwrap_err();
        assert!(matches!(server_error, NetError::Unauthorized(_)));
        let closed: Result<Option<serde_json::Value>, _> =
            onlyne_frame::read_frame(&mut right).await;
        assert!(closed.map(|item| item.is_none()).unwrap_or(false));

        let (mut left, mut right) = duplex(16 * 1024);
        let server_table = table.clone();
        let server = tokio::spawn(async move {
            accept_with_timeout(&mut left, &server_table, 1, timeout_limit).await
        });
        let error = offer_with_timeout(
            &mut right,
            "worker",
            &tamper_key,
            1,
            "agent",
            "1.0",
            false,
            timeout_limit,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, NetError::Rejected { code, .. } if code == "unauthorized"));
        let server_error = server.await.unwrap().unwrap_err();
        assert!(matches!(server_error, NetError::Unauthorized(_)));
        let (mut left, mut right) = duplex(16 * 1024);
        let server_table = table.clone();
        let server = tokio::spawn(async move {
            accept_with_timeout(&mut left, &server_table, 1, timeout_limit).await
        });
        let error = offer_with_timeout(
            &mut right,
            "worker",
            &registered,
            2,
            "agent",
            "1.0",
            false,
            timeout_limit,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, NetError::Rejected { code, .. } if code == "protocol_version"));
        let server_error = server.await.unwrap().unwrap_err();
        assert!(matches!(
            server_error,
            NetError::ProtocolVersion {
                peer: 2,
                expected: 1
            }
        ));
    }

    #[tokio::test]
    async fn handshake_rejects_unregistered_key_before_request() {
        let registered = KeyPair::from_seed([31; 32]);
        let foreign = KeyPair::from_seed([32; 32]);
        let table = table_from([(
            "worker".to_string(),
            registered.public_str(),
            false,
            vec!["*".to_string()],
            vec!["*".to_string()],
        )])
        .unwrap();
        let (mut left, mut right) = duplex(16 * 1024);
        let server_table = table.clone();
        let server = tokio::spawn(async move { accept(&mut left, &server_table, 1).await });
        let error = offer(&mut right, "worker", &foreign, 1, "agent", "1.0", false)
            .await
            .unwrap_err();
        assert!(matches!(error, NetError::Rejected { code, .. } if code == "unauthorized"));
        let server_error = server.await.unwrap().unwrap_err();
        assert!(matches!(server_error, NetError::Unauthorized(_)));
        let closed: Result<Option<serde_json::Value>, _> =
            onlyne_frame::read_frame(&mut right).await;
        assert!(closed.map(|item| item.is_none()).unwrap_or(false));
    }

    #[tokio::test]
    async fn handshake_rejects_protocol_version_mismatch() {
        let registered = KeyPair::from_seed([33; 32]);
        let table = table_from([(
            "worker".to_string(),
            registered.public_str(),
            false,
            vec!["*".to_string()],
            vec!["*".to_string()],
        )])
        .unwrap();
        let timeout_limit = Duration::from_secs(5);
        let (mut left, mut right) = duplex(16 * 1024);
        let server_table = table.clone();
        let server = tokio::spawn(async move {
            accept_with_timeout(&mut left, &server_table, 1, timeout_limit).await
        });
        let error = offer_with_timeout(
            &mut right,
            "worker",
            &registered,
            2,
            "agent",
            "1.0",
            false,
            timeout_limit,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, NetError::Rejected { code, .. } if code == "protocol_version"));
        let server_error = server.await.unwrap().unwrap_err();
        assert!(matches!(
            server_error,
            NetError::ProtocolVersion {
                peer: 2,
                expected: 1
            }
        ));
    }

    #[tokio::test]
    async fn handshake_rejects_tampered_challenge() {
        use onlyne_frame::{read_frame, write_frame};
        let registered = KeyPair::from_seed([21; 32]);
        let table = table_from([(
            "worker".to_string(),
            registered.public_str(),
            false,
            vec!["*".to_string()],
            vec!["*".to_string()],
        )])
        .unwrap();
        let (mut left, mut right) = duplex(16 * 1024);
        let server = tokio::spawn(async move { accept(&mut left, &table, 1).await });
        let challenge: serde_json::Value = read_frame(&mut right).await.unwrap().unwrap();
        assert_eq!(challenge["t"], "challenge");
        let wrong = registered.sign(&challenge_message(&[9u8; 32], "worker", 1));
        write_frame(
            &mut right,
            &serde_json::json!({
                "role": "worker",
                "key": registered.public_str(),
                "signature": wrong,
                "agent": "agent",
                "version": "1.0",
                "aggregate": false,
                "protocol": 1,
            }),
        )
        .await
        .unwrap();
        let ack: serde_json::Value = read_frame(&mut right).await.unwrap().unwrap();
        assert_eq!(ack["ok"], false);
        assert_eq!(ack["code"], "unauthorized");
        let server_error = server.await.unwrap().unwrap_err();
        assert!(matches!(server_error, NetError::Unauthorized(_)));
    }

    #[tokio::test]
    async fn handshake_times_out_without_hello() {
        let registered = KeyPair::from_seed([22; 32]);
        let table = table_from([(
            "worker".to_string(),
            registered.public_str(),
            false,
            vec!["*".to_string()],
            vec!["*".to_string()],
        )])
        .unwrap();
        let (mut left, _right) = duplex(16 * 1024);
        let error = accept_with_timeout(&mut left, &table, 1, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(matches!(error, NetError::HandshakeTimeout));
    }

    #[test]
    fn identity_round_trip_and_rejects_bad_prefix() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("seed");
        let key = KeyPair::from_seed([23; 32]);
        key.save(&path).unwrap();
        let loaded = KeyPair::load(&path).unwrap();
        assert_eq!(loaded.public_str(), key.public_str());
        let error = parse_public("rsa/AAAA").unwrap_err();
        assert!(matches!(error, NetError::MalformedKey(_)));
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
            stream
                .send_frame(&serde_json::json!({"side":"client"}))
                .await
                .unwrap();
            stream
                .recv_frame::<serde_json::Value>()
                .await
                .unwrap()
                .unwrap()
        });
        let mut server = listener.accept_next(&config).await.unwrap();
        let request = server
            .recv_frame::<serde_json::Value>()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request["side"], "client");
        server
            .send_frame(&serde_json::json!({"side":"server"}))
            .await
            .unwrap();
        assert_eq!(client.await.unwrap()["side"], "server");
    }

    #[tokio::test]
    async fn tls_wrong_pin_reports_both_values() {
        let cert = gen_self_signed("127.0.0.1", 1).unwrap();
        let config = server_config(&cert).unwrap();
        let mut listener = TcpListen::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let expected = format!("sha256/{}", "A".repeat(43));
        let wrong = expected.clone();
        let client =
            tokio::spawn(async move { TlsConn::connect(&address.to_string(), &wrong).await });
        let _ = listener.accept_next(&config).await;
        match client.await.unwrap() {
            Err(NetError::PinMismatch {
                expected: got_expected,
                got,
            }) => {
                assert_eq!(got_expected, expected);
                assert_eq!(got, cert.spki_pin);
            }
            result => panic!("unexpected result: {result:?}"),
        }
    }
}
