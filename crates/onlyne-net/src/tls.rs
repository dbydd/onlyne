use ::time::{Duration, OffsetDateTime};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rcgen::{CertificateParams, DnType, KeyPair as RcgenKeyPair};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, DigitallySignedStruct, Error as TlsError, RootCertStore, ServerConfig,
    SignatureScheme,
};
use std::error::Error as StdError;
use std::fmt;
use std::fs;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;
use x509_parser::prelude::parse_x509_certificate;

use crate::NetError;

#[derive(Debug, Clone)]
pub struct ServerCert {
    pub key_pem: Vec<u8>,
    pub cert_pem: Vec<u8>,
    pub spki_pin: String,
}

#[derive(Debug)]
pub(crate) struct PinMismatchMarker {
    pub expected: String,
    pub got: String,
}

impl fmt::Display for PinMismatchMarker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "certificate pin mismatch: expected {}, got {}",
            self.expected, self.got
        )
    }
}

impl StdError for PinMismatchMarker {}

pub fn gen_self_signed(common_name: &str, validity_days: i64) -> Result<ServerCert, NetError> {
    if common_name.is_empty() {
        return Err(NetError::Crypto(
            "common name must not be empty".to_string(),
        ));
    }
    if validity_days <= 0 {
        return Err(NetError::Crypto(
            "validity_days must be positive".to_string(),
        ));
    }
    let now = OffsetDateTime::now_utc();
    let mut params = CertificateParams::new(vec![common_name.to_string()])
        .map_err(|error| NetError::Crypto(error.to_string()))?;
    params.not_before = now - Duration::minutes(1);
    params.not_after = now + Duration::days(validity_days);
    params
        .distinguished_name
        .push(DnType::CommonName, common_name);
    let key_pair = RcgenKeyPair::generate().map_err(|error| NetError::Crypto(error.to_string()))?;
    let cert = params
        .self_signed(&key_pair)
        .map_err(|error| NetError::Crypto(error.to_string()))?;
    let cert_pem = cert.pem().into_bytes();
    let key_pem = key_pair.serialize_pem().into_bytes();
    let spki_pin = spki_pin_of(cert.der().as_ref())?;
    Ok(ServerCert {
        key_pem,
        cert_pem,
        spki_pin,
    })
}

pub fn load_or_create(path: &Path, common_name: &str) -> Result<ServerCert, NetError> {
    if path.exists() {
        if let Ok(cert) = load_pair(path) {
            return Ok(cert);
        }
    }
    let cert = gen_self_signed(common_name, 3650)?;
    let mut data = Vec::with_capacity(cert.key_pem.len() + cert.cert_pem.len() + 1);
    data.extend_from_slice(&cert.key_pem);
    if !cert.key_pem.ends_with(b"\n") {
        data.push(b'\n');
    }
    data.extend_from_slice(&cert.cert_pem);
    fs::write(path, data)?;
    set_private_mode(path)?;
    Ok(cert)
}

fn load_pair(path: &Path) -> Result<ServerCert, NetError> {
    let bytes = fs::read(path)?;
    let mut reader = BufReader::new(bytes.as_slice());
    let items: Vec<_> = rustls_pemfile::read_all(&mut reader).collect::<Result<Vec<_>, _>>()?;
    let cert = items
        .iter()
        .find_map(|item| match item {
            rustls_pemfile::Item::X509Certificate(cert) => Some(cert.to_vec()),
            _ => None,
        })
        .ok_or_else(|| NetError::Crypto("certificate file has no certificate".to_string()))?;
    let (_, parsed) =
        parse_x509_certificate(&cert).map_err(|error| NetError::Crypto(error.to_string()))?;
    if !parsed.validity().is_valid() {
        return Err(NetError::Crypto(
            "stored certificate is expired or not yet valid".to_string(),
        ));
    }
    Ok(ServerCert {
        key_pem: extract_first_key_pem(&bytes)?,
        cert_pem: extract_first_cert_pem(&bytes)?,
        spki_pin: spki_pin_of(&cert)?,
    })
}

fn extract_first_key_pem(bytes: &[u8]) -> Result<Vec<u8>, NetError> {
    for marker in [
        b"-----BEGIN PRIVATE KEY-----".as_slice(),
        b"-----BEGIN RSA PRIVATE KEY-----",
        b"-----BEGIN EC PRIVATE KEY-----",
    ] {
        if let Some(start) = bytes
            .windows(marker.len())
            .position(|window| window == marker)
        {
            if let Some(end_rel) = bytes[start..]
                .windows(b"-----END ".len())
                .position(|window| window == b"-----END ")
            {
                let end_start = start + end_rel;
                let end = bytes[end_start..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(bytes.len(), |offset| end_start + offset + 1);
                return Ok(bytes[start..end].to_vec());
            }
        }
    }
    Err(NetError::Crypto("private key PEM is malformed".to_string()))
}

fn extract_first_cert_pem(bytes: &[u8]) -> Result<Vec<u8>, NetError> {
    let begin = b"-----BEGIN CERTIFICATE-----";
    let end_marker = b"-----END CERTIFICATE-----";
    let start = bytes
        .windows(begin.len())
        .position(|window| window == begin)
        .ok_or_else(|| NetError::Crypto("certificate PEM is malformed".to_string()))?;
    let end_rel = bytes[start..]
        .windows(end_marker.len())
        .position(|window| window == end_marker)
        .ok_or_else(|| NetError::Crypto("certificate PEM is malformed".to_string()))?;
    let end_start = start + end_rel;
    let end = bytes[end_start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(bytes.len(), |offset| end_start + offset + 1);
    Ok(bytes[start..end].to_vec())
}

pub fn spki_pin_of(der_cert: &[u8]) -> Result<String, NetError> {
    let (_, certificate) = parse_x509_certificate(der_cert)
        .map_err(|error| NetError::Crypto(format!("invalid DER certificate: {error}")))?;
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(certificate.tbs_certificate.subject_pki.raw);
    Ok(format!("sha256/{}", STANDARD.encode(digest)))
}

pub fn server_config(cert: &ServerCert) -> Result<ServerConfig, NetError> {
    let mut cert_reader = BufReader::new(cert.cert_pem.as_slice());
    let certs = rustls_pemfile::certs(&mut cert_reader).collect::<Result<Vec<_>, _>>()?;
    let mut key_reader = BufReader::new(cert.key_pem.as_slice());
    let key = rustls_pemfile::private_key(&mut key_reader)?
        .ok_or_else(|| NetError::Crypto("private key PEM is missing".to_string()))?;
    rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|error| NetError::Crypto(error.to_string()))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|error| NetError::Crypto(error.to_string()))
}

/// Build a TLS 1.3 client that accepts only the pinned leaf certificate.
///
/// Pin equality plus a self-issued leaf means the peer proved possession of the pinned key.
pub fn client_config(expected_pin: &str) -> Result<ClientConfig, NetError> {
    if !expected_pin.starts_with("sha256/") {
        return Err(NetError::MalformedKey(
            "expected certificate pin must use sha256/ prefix".to_string(),
        ));
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(PinnedVerifier {
        expected: expected_pin.to_string(),
        provider: provider.clone(),
    });
    rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|error| NetError::Crypto(error.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth()
        .pipe(Ok)
}

/// Pin verifier holds the expected SPKI pin and the ring provider.
///
/// `WebPkiVerifier` runs the full chain check with the leaf as trust anchor.
#[derive(Debug)]
struct PinnedVerifier {
    expected: String,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let got = spki_pin_of(end_entity.as_ref())
            .map_err(|_| TlsError::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
        if got != self.expected {
            return Err(TlsError::Other(rustls::OtherError(Arc::new(
                PinMismatchMarker {
                    expected: self.expected.clone(),
                    got,
                },
            ))));
        }
        let mut roots = RootCertStore::empty();
        roots
            .add(end_entity.clone())
            .map_err(|_| TlsError::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
        let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            self.provider.clone(),
        )
        .build()
        .map_err(|error| TlsError::General(error.to_string()))?;
        verifier.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Err(TlsError::General("TLS 1.2 is disabled".to_string()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        let mut roots = RootCertStore::empty();
        roots
            .add(cert.clone())
            .map_err(|_| TlsError::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
        let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            self.provider.clone(),
        )
        .build()
        .map_err(|error| TlsError::General(error.to_string()))?;
        verifier.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn set_private_mode(path: &Path) -> Result<(), NetError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T;
}
impl<T> Pipe for T {
    fn pipe<U>(self, f: impl FnOnce(Self) -> U) -> U {
        f(self)
    }
}
