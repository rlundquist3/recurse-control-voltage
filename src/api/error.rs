use defmt::Format;
use embassy_net::{dns, tcp::ConnectError};
use embedded_tls::TlsError;

#[derive(Debug, Format)]
pub enum Error {
    Dns(dns::Error),
    NoAddress,
    Connect(ConnectError),
    Tls(TlsError),
    WebSocket(edge_ws::Error<TlsError>),
    Handshake,
    Rejected,
    Disconnected,
}

impl From<dns::Error> for Error {
    fn from(e: dns::Error) -> Self {
        Self::Dns(e)
    }
}

impl From<ConnectError> for Error {
    fn from(e: ConnectError) -> Self {
        Self::Connect(e)
    }
}

impl From<TlsError> for Error {
    fn from(e: TlsError) -> Self {
        Self::Tls(e)
    }
}

impl From<edge_ws::Error<TlsError>> for Error {
    fn from(e: edge_ws::Error<TlsError>) -> Self {
        Self::WebSocket(e)
    }
}
