use core::str::from_utf8;
use defmt::{Format, info, warn};
use edge_ws::{FrameHeader, FrameType};
use embassy_net::{
    Stack,
    dns::{self, DnsQueryType},
    tcp::{ConnectError, TcpSocket},
};
use embassy_rp::clocks::RoscRng;
use embassy_time::{Duration, Timer};
use embedded_io_async::{Read, Write};
use embedded_tls::{
    Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, TlsError, UnsecureProvider,
};
use serde::Deserialize;

use crate::api::error::Error;

mod error;

const HOST: &str = "recurse.rctogether.com";
const PORT: u16 = 443;
const ORIGIN: &str = "https://example.rctogether.com";
const APP_ID: &str = env!("APP_ID");
const APP_SECRET: &str = env!("APP_SECRET");

const SUBSCRIBE: &str = r#"{"command":"subscribe","identifier":"{\"channel\":\"ApiChannel\"}"}"#;

const READ_TIMEOUT: Duration = Duration::from_secs(10);
const RECONNECT_DELAY: Duration = Duration::from_secs(10);

type Tls<'a> = TlsConnection<'a, TcpSocket<'a>, Aes128GcmSha256>;

struct Buffers {
    tcp_rx: [u8; 4096],
    tcp_tx: [u8; 4096],
    tls_rx: [u8; 16640],
    tls_tx: [u8; 4096],
    frame: [u8; 2048], // largest I've seen is 1019 bytes - keep an eye on it
}

pub async fn run(stack: Stack<'static>) -> ! {
    let mut buffers = Buffers {
        tcp_rx: [0; 4096],
        tcp_tx: [0; 4096],
        tls_rx: [0; 16640],
        tls_tx: [0; 4096],
        frame: [0; 2048],
    };

    loop {
        match session(stack, &mut buffers).await {
            Ok(()) => info!("api: connection closed by server"),
            Err(err) => warn!("api: {:?}", err),
        }
        Timer::after(RECONNECT_DELAY).await;
    }
}

async fn session(stack: Stack<'static>, buffers: &mut Buffers) -> Result<(), Error> {
    let address = *stack
        .dns_query(HOST, DnsQueryType::A)
        .await?
        .first()
        .ok_or(Error::NoAddress)?;

    let mut socket = TcpSocket::new(stack, &mut buffers.tcp_rx, &mut buffers.tcp_tx);
    socket.set_timeout(Some(READ_TIMEOUT));
    socket.connect((address, PORT)).await?;
    info!("api: tcp connected to {}", address);

    let config = TlsConfig::new().with_server_name(HOST);
    let mut tls = TlsConnection::new(socket, &mut buffers.tls_rx, &mut buffers.tls_tx);
    tls.open(TlsContext::new(
        &config,
        UnsecureProvider::new::<Aes128GcmSha256>(RoscRng),
    ))
    .await?;
    info!("api: tls open");

    let frame = &mut buffers.frame;
    upgrade_to_websocket(&mut tls, frame).await?;
    info!("api: websocket open");

    loop {
        let header = FrameHeader::recv(&mut tls).await?;

        if header.payload_len > frame.len() as u64 {
            warn!("api: skipping {} byte frame", header.payload_len);
            discard_frame(&mut tls, header.payload_len, frame).await?;
            continue;
        }

        let payload = header.recv_payload(&mut tls, frame).await?;

        match header.frame_type {
            FrameType::Text(false) => match from_utf8(payload) {
                Ok(json) => handle_message(&mut tls, json).await?,
                Err(_) => warn!("api: text frame not utf-8"),
            },
            FrameType::Close => return Ok(()),
            FrameType::Ping => send_frame(&mut tls, FrameType::Pong, payload).await?,
            FrameType::Pong => {}
            frame_type => warn!("api: ignoring {:?} frame", frame_type),
        }
    }
}

async fn upgrade_to_websocket(tls: &mut Tls<'_>, buffer: &mut [u8]) -> Result<(), Error> {
    let request = [
        "GET /cable?app_id=",
        APP_ID,
        "&app_secret=",
        APP_SECRET,
        " HTTP/1.1\r\n",
        "Host: ",
        HOST,
        "\r\n",
        "Origin: ",
        ORIGIN,
        "\r\n",
        "Upgrade: websocket\r\n",
        "Connection: Upgrade\r\n",
        "Sec-WebSocket-Version: 13\r\n",
        "Sec-WebSocket-Key: eFjnHbXZuB28QhxGb71bZG==\r\n",
        "Sec-WebSocket-Protocol: actioncable-v1-json\r\n",
        "\r\n",
    ];
    for part in request {
        tls.write_all(part.as_bytes()).await?;
    }
    tls.flush().await?;

    let mut len = 0;
    while !buffer[..len].ends_with(b"\r\n\r\n") {
        if len == buffer.len() || tls.read(&mut buffer[len..len + 1]).await? == 0 {
            return Err(Error::Handshake);
        }
        len += 1;
    }

    if !buffer.starts_with(b"HTTP/1.1 101") {
        let status = buffer[..len]
            .split(|&b| b == b'\r')
            .next()
            .unwrap_or_default();
        warn!(
            "api: websocket upgrade refused: {}",
            from_utf8(status).unwrap_or("unknown")
        );
        return Err(Error::Handshake);
    }

    Ok(())
}

#[derive(Deserialize)]
struct ActionCableMessage<'a> {
    #[serde(rename = "type", borrow)]
    message_type: Option<&'a str>,
}

#[derive(Deserialize)]
struct ApiChannelMessage<'a> {
    #[serde(rename = "type", borrow)]
    frame_type: Option<&'a str>,
    #[serde(borrow)]
    message: Message<'a>,
}

#[derive(Deserialize)]
struct Message<'a> {
    #[serde(rename = "type")]
    message_type: &'a str,
    #[serde(borrow)]
    payload: Option<Entity<'a>>,
}

#[derive(Debug, Deserialize)]
struct Entity<'a> {
    id: u32,
    #[serde(rename = "type")]
    entity_type: &'a str,
    pos: Position,
    #[serde(borrow)]
    direction: Option<&'a str>,
    #[serde(borrow)]
    person_name: Option<&'a str>,
    #[serde(borrow)]
    name: Option<&'a str>,
    #[serde(borrow)]
    note_text: Option<&'a str>,
    #[serde(borrow)]
    color: Option<&'a str>,
    participant_count: Option<u8>,
}

#[derive(Debug, Deserialize)]
struct Position {
    x: i32,
    y: i32,
}

async fn handle_message(tls: &mut Tls<'_>, json: &str) -> Result<(), Error> {
    let message = match serde_json_core::from_str::<ActionCableMessage>(json) {
        Ok((message, _)) => message,
        Err(err) => {
            warn!("api: could not parse message: {:?}", err);
            return Ok(());
        }
    };

    match message.message_type {
        Some("welcome") => {
            info!("api: welcome, subscribing");
            send_frame(tls, FrameType::Text(false), SUBSCRIBE.as_bytes()).await?;
        }
        Some("ping") => {}
        Some("confirm_subscription") => info!("api: subscribed to ApiChannel"),
        Some("reject_subscription") => return Err(Error::Rejected),
        Some("disconnect") => return Err(Error::Disconnected),
        Some(other) => info!("api: unhandled type {}", other),
        None => match serde_json_core::from_str::<ApiChannelMessage>(json) {
            Ok((message, _)) => {
                if message.message.message_type == "entity" {
                    if let Some(entity) = message.message.payload {
                        let name = match entity.entity_type {
                            "Avatar" => entity.person_name,
                            "Bot" => entity.name,
                            _ => Some("unknown"),
                        };
                        match name {
                            Some(name) => info!("entity message for {:?}", name),
                            None => {}
                        }
                    }
                } else {
                    info!("skipping world message");
                }
            }
            Err(err) => warn!("api: could not parse channel message: {:?}", err),
        },
    }
    Ok(())
}

async fn send_frame(tls: &mut Tls<'_>, frame_type: FrameType, payload: &[u8]) -> Result<(), Error> {
    let mask = RoscRng.next_u32();
    edge_ws::io::send(&mut *tls, frame_type, Some(mask), payload).await?;
    tls.flush().await?;
    Ok(())
}

async fn discard_frame(
    tls: &mut Tls<'_>,
    mut remaining: u64,
    temp: &mut [u8],
) -> Result<(), Error> {
    while remaining > 0 {
        let chunk = remaining.min(temp.len() as u64) as usize;
        tls.read_exact(&mut temp[..chunk])
            .await
            .map_err(edge_ws::Error::from)?;
        remaining -= chunk as u64;
    }
    Ok(())
}
