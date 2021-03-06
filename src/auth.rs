//! A wrapper for performing Remote Framebuffer authentication.

use anyhow::{bail, Context, Result};

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::protocol::Message as WebSocketMessage;

/// What kind of authentication to use for an RFB connection.
pub enum RfbAuthentication {
    /// A null authentication. It does not perform the initial handshake, so it relies on the rest
    /// of the connection to pass through the data as-is without parsing.
    Null,

    /// An authentication that parses the initial ProtocolVersion and Security handshakes, and
    /// passes them as-is to the peer, without acting on it. This leaves the stream in a state
    /// where the ClientInit handshake is expected to appear next, followed by a stream of normal
    /// RFB messages can appear.
    Passthrough,
}

/// A way of authenticating an RFB connection.
pub async fn authenticate<SocketStream, WebSocketStream>(
    authentication: &RfbAuthentication,
    stream: &mut SocketStream,
    ws_stream: &mut WebSocketStream,
) -> Result<()>
where
    SocketStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
    WebSocketStream: futures::Sink<WebSocketMessage, Error = tokio_tungstenite::tungstenite::error::Error>
        + futures::Stream<
            Item = std::result::Result<
                WebSocketMessage,
                tokio_tungstenite::tungstenite::error::Error,
            >,
        > + Unpin
        + Send,
{
    match authentication {
        RfbAuthentication::Null => Ok(()),
        RfbAuthentication::Passthrough => authenticate_passthrough(stream, ws_stream).await,
    }
}

async fn authenticate_passthrough<SocketStream, WebSocketStream>(
    stream: &mut SocketStream,
    ws_stream: &mut WebSocketStream,
) -> Result<()>
where
    SocketStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
    WebSocketStream: futures::Sink<WebSocketMessage, Error = tokio_tungstenite::tungstenite::error::Error>
        + futures::Stream<
            Item = std::result::Result<
                WebSocketMessage,
                tokio_tungstenite::tungstenite::error::Error,
            >,
        > + Unpin
        + Send,
{
    let mut buf = [0u8; 1024];

    // ProtocolVersion handshake.
    let n = stream.read(&mut buf[0..12]).await?;
    if n != 12 {
        bail!("unexpected server handshake: {:?}", &buf[..n]);
    }
    log::debug!("<-: {:?}", std::str::from_utf8(&buf[0..12])?);
    ws_stream
        .send(WebSocketMessage::Binary(buf[0..12].to_vec()))
        .await?;
    match ws_stream.next().await {
        Some(msg) => match msg.context("bad client ProtocolVersion handshake")? {
            WebSocketMessage::Binary(payload) => {
                log::debug!("->: {:?}", std::str::from_utf8(&payload)?);
                stream.write_all(&payload).await?;
            }
            unexpected_msg => bail!("unexpected message {:?}", unexpected_msg),
        },
        None => bail!("missing client ProtocolVersion handshake"),
    }

    // Security handshake.
    let mut n = stream.read(&mut buf).await?;
    log::debug!("<-: {:?}", &buf[0..n]);
    ws_stream
        .send(WebSocketMessage::Binary(buf[0..n].to_vec()))
        .await?;
    let client_security_handshake = match ws_stream.next().await {
        Some(msg) => match msg.context("bad client security handshake")? {
            WebSocketMessage::Binary(payload) => {
                if payload.len() != 1 {
                    bail!(
                        "unexpected security-type length. got {}, expected 1",
                        payload.len()
                    );
                }
                log::debug!("->: {:?}", &payload);
                stream.write_all(&payload).await?;
                payload[0]
            }
            unexpected_msg => bail!("unexpected message {:?}", unexpected_msg),
        },
        None => bail!("missing client security handshake"),
    };
    match client_security_handshake {
        1 => {
            // None security type
        }
        2 => {
            // VNC Authentication security type
            n = stream.read(&mut buf).await?;
            log::debug!("<-: {:?}", &buf[0..n]);
            ws_stream
                .send(WebSocketMessage::Binary(buf[0..n].to_vec()))
                .await?;
            match ws_stream.next().await {
                Some(msg) => match msg.context("bad client VNCAuth security handshake")? {
                    WebSocketMessage::Binary(payload) => {
                        log::debug!("->: {:?}", &payload);
                        stream.write_all(&payload).await?;
                    }
                    unexpected_msg => bail!("unexpected message {:?}", unexpected_msg),
                },
                None => bail!("missing client VNCAuth security handshake"),
            }
        }
        unsupported => bail!("unsupported security type {}", unsupported),
    }

    // SecurityResult handshake.
    n = stream.read(&mut buf).await?;
    log::debug!("<-: {:?}", &buf[0..n]);
    ws_stream
        .send(WebSocketMessage::Binary(buf[0..n].to_vec()))
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio_test::io::Builder;

    fn init() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    #[test]
    fn test_passthrough_none_security_type() {
        init();

        let mut socket_mock = Builder::new()
            .read(b"RFB 003.008\n")
            .write(b"RFB 003.008\n")
            // Only the None(1) security type is supported.
            .read(b"\x01\x01")
            .write(b"\x01")
            // Success!
            .read(b"\x00\x00\x00\x00")
            .build();
        // Acting as a server to avoid having to unmask the frames.
        let mut websocket_stream =
            tokio_test::block_on(tokio_tungstenite::WebSocketStream::from_raw_socket(
                Builder::new()
                    .write(b"\x82\x0cRFB 003.008\n")
                    .read(b"\x82\x0cRFB 003.008\n")
                    .write(b"\x82\x02\x01\x01")
                    .read(b"\x82\x01\x01")
                    .write(b"\x82\x04\x00\x00\x00\x00")
                    .build(),
                tokio_tungstenite::tungstenite::protocol::Role::Server,
                Some(tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
                    max_send_queue: None,
                    max_message_size: None,
                    max_frame_size: None,
                    accept_unmasked_frames: true,
                }),
            ));

        tokio_test::block_on(authenticate_passthrough(
            &mut socket_mock,
            &mut websocket_stream,
        ))
        .expect("could not authenticate");
    }

    #[test]
    fn test_passthrough_vncauth_security_type() {
        init();

        let mut socket_mock = Builder::new()
            .read(b"RFB 003.008\n")
            .write(b"RFB 003.008\n")
            // Only the VncAuth(2) security type is supported.
            .read(b"\x01\x02")
            .write(b"\x02")
            // Challenge + Response. The password is, unsurprisingly, "password".
            .read(b"\x9e\xdd\x1d\xc2\xee\x5a\x5e\x78\x7f\x55\x21\xf2\x67\x9f\x71\xd6")
            .write(b"\x15\x6d\x69\xd7\x0f\x22\x21\xb5\x6f\x46\xe2\x92\xa3\xe2\x68\x37")
            // Success!
            .read(b"\x00\x00\x00\x00")
            .build();
        // Acting as a server to avoid having to unmask the frames.
        let mut websocket_stream =
            tokio_test::block_on(tokio_tungstenite::WebSocketStream::from_raw_socket(
                Builder::new()
                    .write(b"\x82\x0cRFB 003.008\n")
                    .read(b"\x82\x0cRFB 003.008\n")
                    .write(b"\x82\x02\x01\x02")
                    .read(b"\x82\x01\x02")
                    .write(
                        b"\x82\x10\x9e\xdd\x1d\xc2\xee\x5a\x5e\x78\x7f\x55\x21\xf2\x67\x9f\x71\xd6",
                    )
                    .read(
                        b"\x82\x10\x15\x6d\x69\xd7\x0f\x22\x21\xb5\x6f\x46\xe2\x92\xa3\xe2\x68\x37",
                    )
                    .write(b"\x82\x04\x00\x00\x00\x00")
                    .build(),
                tokio_tungstenite::tungstenite::protocol::Role::Server,
                Some(tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
                    max_send_queue: None,
                    max_message_size: None,
                    max_frame_size: None,
                    accept_unmasked_frames: true,
                }),
            ));

        tokio_test::block_on(authenticate_passthrough(
            &mut socket_mock,
            &mut websocket_stream,
        ))
        .expect("could not authenticate");
    }
}
