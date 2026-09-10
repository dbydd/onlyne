use onlyne_frame::{is_bad_frame, is_too_large, read_frame, write_frame};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, Error as TlsError, ServerConfig};
use serde::{de::DeserializeOwned, Serialize};
use std::error::Error as StdError;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::tls::{client_config, PinMismatchMarker};
use crate::NetError;

#[derive(Debug)]
pub enum TlsConn {
    Client(tokio_rustls::client::TlsStream<TcpStream>),
    Server(tokio_rustls::server::TlsStream<TcpStream>),
}

impl TlsConn {
    pub async fn connect(endpoint: &str, pin: &str) -> Result<Self, NetError> {
        let config = client_config(pin)?;
        Self::connect_with_config(endpoint, config).await
    }

    pub async fn connect_with_config(endpoint: &str, config: ClientConfig) -> Result<Self, NetError> {
        let (host, stream) = connect_tcp(endpoint).await?;
        let server_name = ServerName::try_from(host.clone()).map_err(|_| NetError::Io(format!("invalid TLS server name {host}")))?;
        let connector = TlsConnector::from(Arc::new(config));
        connector.connect(server_name, stream).await.map(Self::Client).map_err(map_tls_io_error)
    }

    pub async fn send_frame<T: Serialize>(&mut self, value: &T) -> Result<(), NetError> {
        match self {
            Self::Client(stream) => write_frame(stream, value).await,
            Self::Server(stream) => write_frame(stream, value).await,
        }.map_err(map_frame_error)
    }

    pub async fn recv_frame<T: DeserializeOwned>(&mut self) -> Result<Option<T>, NetError> {
        match self {
            Self::Client(stream) => read_frame(stream).await,
            Self::Server(stream) => read_frame(stream).await,
        }.map_err(map_frame_error)
    }

    pub fn peer_closed<T>(value: &Option<T>) -> bool {
        value.is_none()
    }
}

#[derive(Debug)]
pub struct TcpListen {
    listener: TcpListener,
}

impl TcpListen {
    pub async fn bind(listen: &str) -> Result<Self, NetError> {
        let listener = TcpListener::bind(listen).await?;
        Ok(Self { listener })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        Ok(self.listener.local_addr()?)
    }

    pub async fn accept_next(&mut self, config: &ServerConfig) -> Result<TlsConn, NetError> {
        let (stream, _) = self.listener.accept().await?;
        let acceptor = TlsAcceptor::from(Arc::new(config.clone()));
        acceptor.accept(stream).await.map(TlsConn::Server).map_err(map_tls_io_error)
    }
}

async fn connect_tcp(endpoint: &str) -> Result<(String, TcpStream), NetError> {
    let (host, _) = if let Ok(address) = endpoint.parse::<SocketAddr>() {
        (address.ip().to_string(), address.port())
    } else {
        let (host, port) = endpoint.rsplit_once(':').ok_or_else(|| NetError::Io(format!("invalid endpoint {endpoint}")))?;
        (host.trim_matches(['[', ']']).to_string(), port.parse::<u16>().map_err(|_| NetError::Io(format!("invalid endpoint {endpoint}")))?)
    };
    let mut addresses = tokio::net::lookup_host(endpoint).await?;
    let address = addresses.next().ok_or_else(|| NetError::Io(format!("endpoint {endpoint} has no addresses")))?;
    Ok((host, TcpStream::connect(address).await?))
}

fn map_frame_error(error: std::io::Error) -> NetError {
    if is_too_large(&error) {
        NetError::FrameTooLarge
    } else if is_bad_frame(&error) {
        NetError::BadFrame
    } else {
        NetError::Io(error.to_string())
    }
}

fn map_tls_io_error(error: std::io::Error) -> NetError {
    if let Some(source) = error.get_ref().and_then(|source| source.downcast_ref::<TlsError>()) {
        return map_rustls_error(source);
    }
    NetError::Io(error.to_string())
}

fn map_rustls_error(error: &TlsError) -> NetError {
    if let TlsError::Other(other) = error {
        if let Some(marker) = other.source().and_then(|source| source.downcast_ref::<PinMismatchMarker>()) {
            return NetError::PinMismatch { expected: marker.expected.clone(), got: marker.got.clone() };
        }
    }
    NetError::Io(error.to_string())
}
