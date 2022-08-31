//! A Remote Framebuffer connection that can inject audio messages into the stream.
//!
//! This connection also translates from a TCP socket (server) to a Websocket (client) for use with
//! noVNC.

use crate::audio;
use crate::messages;
use crate::messages::{client, server};

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};

use bytes::{Buf, BytesMut};

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

/// The shared state between the RFB client and server.
#[derive(Debug)]
pub struct RfbConnectionState {
    bytes_per_pixel: AtomicUsize,
}

/// A Remote Framebuffer connection between a TCP server and a Websocket client.
#[derive(Debug)]
#[allow(dead_code)]
pub struct RfbConnection {
    stream: tokio::net::TcpStream,
    connection_state: Arc<RfbConnectionState>,
    server_tx: mpsc::Sender<Vec<u8>>,
    client_tx: mpsc::Sender<Vec<u8>>,
}

impl RfbConnection {
    pub async fn new<Stream>(
        mut stream: tokio::net::TcpStream,
        ws_stream: &mut tokio_tungstenite::WebSocketStream<Stream>,
        server_tx: mpsc::Sender<Vec<u8>>,
        client_tx: mpsc::Sender<Vec<u8>>,
    ) -> Result<RfbConnection>
    where
        Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let mut buf = [0u8; 1024];

        // ClientInit
        match ws_stream.next().await {
            Some(msg) => match msg.context("bad ClientInit handshake")? {
                tokio_tungstenite::tungstenite::protocol::Message::Binary(payload) => {
                    log::debug!("->: {:?}", &payload);
                    stream.write_all(&payload).await?;
                }
                unexpected_msg => bail!("unexpected message {:?}", unexpected_msg),
            },
            None => bail!("missing ClientInit handshake"),
        }

        // ServerInit handshake.
        let n = stream.read(&mut buf).await?;
        let pixel_format = messages::PixelFormat::new(&buf[4..20]);
        log::debug!("<-: {:?}", pixel_format);
        ws_stream
            .send(tokio_tungstenite::tungstenite::protocol::Message::Binary(
                buf[0..n].to_vec(),
            ))
            .await?;

        log::debug!("\n--: Handshake finished\n");

        Ok(RfbConnection {
            stream,
            connection_state: Arc::new(RfbConnectionState {
                bytes_per_pixel: AtomicUsize::new((pixel_format.bits_per_pixel / 8) as usize),
            }),
            server_tx,
            client_tx,
        })
    }

    pub fn split(&mut self) -> (ReadHalf, WriteHalf) {
        let (rs, ws) = self.stream.split();
        (
            ReadHalf {
                read_ws: rs,
                connection_state: Arc::clone(&self.connection_state),
                buf: BytesMut::with_capacity(4096),
            },
            WriteHalf {
                write_ws: ws,
                connection_state: Arc::clone(&self.connection_state),
                buf: BytesMut::with_capacity(1024),
                client_tx: &mut self.client_tx,
                stop_chan: None,
                audio_stream: None,
            },
        )
    }
}

/// The readable half of [`RfbConnection::split`]. Represents the server-to-client half.
#[derive(Debug)]
pub struct ReadHalf<'a> {
    read_ws: tokio::net::tcp::ReadHalf<'a>,
    connection_state: Arc<RfbConnectionState>,
    buf: BytesMut,
}

impl ReadHalf<'_> {
    pub async fn read_server_message(&mut self) -> Result<Option<server::Message>> {
        loop {
            // Attempt to parse a frame from the buffered data. If enough data
            // has been buffered, the frame is returned.
            let mut cur = std::io::Cursor::new(&self.buf[..]);
            if let Some(msg) = match server::Message::parse(
                &mut cur,
                self.connection_state.bytes_per_pixel.load(Ordering::SeqCst),
            ) {
                Ok(msg) => {
                    self.buf.advance(msg.len());
                    Some(msg)
                }
                // There is not enough data present in the read buffer to parse a
                // single frame. We must wait for more data to be received from the
                // socket. Reading from the socket will be done in the statement
                // after this `match`.
                //
                // We do not want to return `Err` from here as this "error" is an
                // expected runtime condition.
                Err(messages::Error::Incomplete) => None,
                // An error was encountered while parsing the frame. The connection
                // is now in an invalid state. Returning `Err` from here will result
                // in the connection being closed.
                Err(e) => {
                    log::error!("<-: !!welp: {:?}", e);
                    return Err(e.into());
                }
            } {
                return Ok(Some(msg));
            }

            if !self.buf.is_empty() {
                log::debug!("<-: [incomplete]: {:?}", &self.buf);
                log::debug!(
                    "    gonna read a bit, the current buffer of size {} is not enough. brb.",
                    self.buf.len()
                );
            }

            // There is not enough buffered data to read a frame. Attempt to
            // read more data from the socket.
            //
            // On success, the number of bytes is returned. `0` indicates "end
            // of stream".
            let read_result = self.read_ws.read_buf(&mut self.buf).await;
            let n = match read_result {
                Ok(0) => {
                    log::warn!("!!: could not read anything D:.");
                    // The remote closed the connection. For this to be a clean
                    // shutdown, there should be no data in the read buffer. If
                    // there is, this means that the peer closed the socket while
                    // sending a frame.
                    if !self.buf.is_empty() {
                        bail!("connection reset by peer");
                    }
                    return Ok(None);
                }
                Ok(n) => n,
                Err(e) => return Err(e.into()),
            };
            log::debug!("<-: okay, let's parse the {} bytes.", n);
        }
    }
}

/// The writable half of [`RfbConnection::split`]. Represents the client-to-server half.
pub struct WriteHalf<'a> {
    write_ws: tokio::net::tcp::WriteHalf<'a>,
    connection_state: Arc<RfbConnectionState>,
    buf: BytesMut,
    client_tx: &'a mut mpsc::Sender<Vec<u8>>,
    stop_chan: Option<oneshot::Receiver<()>>,
    audio_stream: Option<audio::Stream>,
}

impl WriteHalf<'_> {
    pub async fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        let mut cur = std::io::Cursor::new(buf);
        loop {
            if cur.remaining() > 0 {
                if self.buf.capacity() - self.buf.len() < cur.remaining() {
                    let old_capacity = self.buf.capacity();
                    let old_space_available = self.buf.capacity() - self.buf.len();
                    self.buf.reserve(std::cmp::max(cur.remaining(), 4096));
                    log::debug!("had to grow buffer to be able to add more stuff. old capacity: {}, old space availabile: {}, capacity: {}, space available: {}",
                        old_capacity, old_space_available, self.buf.capacity(), self.buf.capacity() - self.buf.len());
                }

                let written = std::cmp::min(self.buf.capacity() - self.buf.len(), cur.remaining());
                if written == 0 {
                    log::warn!(
                        "could not write to the buffer! capacity: {}, space available: {}, remaining: {}",
                        self.buf.capacity(),
                        self.buf.capacity()- self.buf.len(),
                        cur.remaining()
                    );
                } else {
                    self.buf.extend_from_slice(&cur.get_ref()[0..written]);
                    cur.advance(written);
                }
            }

            let result = self.read_client_message().await;
            match result {
                Err(messages::Error::Incomplete) => break,
                Ok(client::Message::ReplitClientAudioStartEncoder(
                    _payload,
                    enabled,
                    channels,
                    codec,
                    kbps,
                )) => {
                    if !enabled {
                        // This drops the sending channel and cause the stream to stop.
                        self.stop_chan.take();
                        self.audio_stream.take();
                        self.client_tx
                            .send(
                                server::Message::ReplitAudioServerMessage(vec![
                                    0xF5, // message-type
                                    0x00, // submessage-type
                                    0x00, 0x01, // message length
                                    0x00, // enabled
                                ])
                                .into_data(),
                            )
                            .await?;
                        continue;
                    }

                    self.audio_stream = match audio::Stream::new(channels, codec, kbps) {
                        Ok(stream) => Some(stream),
                        Err(e) => {
                            log::error!("failed to create an audio stream: {:#}", e);
                            self.client_tx
                                .send(
                                    server::Message::ReplitAudioServerMessage(vec![
                                        0xF5, // message-type
                                        0x00, // submessage-type
                                        0x00, 0x01, // message length
                                        0x00, // enabled
                                    ])
                                    .into_data(),
                                )
                                .await?;
                            continue;
                        }
                    };
                    self.client_tx
                        .send(
                            server::Message::ReplitAudioServerMessage(vec![
                                0xF5, // message-type
                                0x00, // submessage-type
                                0x00, 0x01, // message length
                                0x01, // enabled
                            ])
                            .into_data(),
                        )
                        .await?;
                    continue;
                }
                Ok(client::Message::ReplitClientAudioStartContinuousUpdates(_payload)) => {
                    let audio_stream = match self.audio_stream.take() {
                        Some(stream) => stream,
                        None => {
                            self.client_tx
                                .send(
                                    server::Message::ReplitAudioServerMessage(vec![
                                        0xF5, // message-type
                                        0x02, // submessage-type
                                        0x00, 0x01, // message length
                                        0x00, // enabled
                                    ])
                                    .into_data(),
                                )
                                .await?;
                            continue;
                        }
                    };
                    self.client_tx
                        .send(
                            server::Message::ReplitAudioServerMessage(vec![
                                0xF5, // message-type
                                0x02, // submessage-type
                                0x00, 0x01, // message length
                                0x01, // enabled
                            ])
                            .into_data(),
                        )
                        .await?;
                    let (stop_chan_tx, stop_chan_rx) = oneshot::channel::<()>();
                    self.stop_chan = Some(stop_chan_rx);
                    let chan = self.client_tx.clone();
                    tokio::task::spawn_blocking(move || {
                        audio_stream.run(stop_chan_tx, chan.clone());
                        futures::executor::block_on(async {
                            if let Err(e) = chan
                                .send(
                                    server::Message::ReplitAudioServerMessage(vec![
                                        0xF5, // message-type
                                        0x02, // submessage-type
                                        0x00, 0x01, // message length
                                        0x00, // enabled
                                    ])
                                    .into_data(),
                                )
                                .await
                            {
                                log::error!("failed to notify client of audio closure: {:#}", e);
                            }
                        });
                    });
                }
                Ok(client::Message::SetEncodings(payload)) => {
                    let mut cur = std::io::Cursor::new(&payload[4..]);
                    while cur.has_remaining() {
                        if cur.get_i32() == 0x52706C41 {
                            self.client_tx
                                .send(
                                    server::Message::FramebufferUpdate(vec![
                                        0x00, // message-type
                                        0x00, // padding
                                        0x00, 0x01, // number-of-rectangles
                                        0x00, 0x00, // x-position
                                        0x00, 0x00, // y-position
                                        0x00, 0x00, // width
                                        0x00, 0x00, // height
                                        0x52, 0x70, 0x6C,
                                        0x41, // Replit Audio Pseudo-encoding
                                        0x00, 0x00, // Version
                                        0x00, 0x02, // Number of encodings
                                        0x00, 0x00, // Opus codec, WebM container
                                        0x00, 0x01, // MP3 codec, MPEG-1 container
                                    ])
                                    .into_data(),
                                )
                                .await?;
                        }
                    }

                    self.write_ws.write_all(&payload).await?;
                }
                Ok(client::Message::SetPixelFormat(payload)) => {
                    let pixel_format = messages::PixelFormat::new(&payload[4..20]);
                    log::debug!("->: SetPixelFormat({:?})", pixel_format);
                    self.connection_state
                        .bytes_per_pixel
                        .store(pixel_format.bits_per_pixel as usize / 8, Ordering::SeqCst);
                    self.write_ws.write_all(&payload).await?;
                }
                Ok(msg) => {
                    log::debug!("->: {}", &msg);
                    self.write_ws.write_all(&msg.into_data()).await?;
                }
                Err(err) => {
                    bail!("failed to read client-to-server message: {:?}", err);
                }
            }
        }
        Ok(())
    }

    async fn read_client_message(&mut self) -> Result<client::Message, messages::Error> {
        let mut cur = std::io::Cursor::new(&self.buf[..]);
        match client::Message::parse(&mut cur) {
            Ok(msg) => {
                self.buf.advance(msg.len());
                Ok(msg)
            }
            // There is not enough data present in the read buffer to parse a
            // single frame. We must wait for more data to be received from the
            // socket. Reading from the socket will be done in the statement
            // after this `match`.
            //
            // We do not want to return `Err` from here as this "error" is an
            // expected runtime condition.
            Err(messages::Error::Incomplete) => {
                if !self.buf.is_empty() {
                    log::debug!("->: [incomplete]: {:?}", &self.buf);
                }
                Err(messages::Error::Incomplete)
            }
            // An error was encountered while parsing the frame. The connection
            // is now in an invalid state. Returning `Err` from here will result
            // in the connection being closed.
            Err(e) => {
                log::error!("->: !! welp: {:?}", e);
                Err(e)
            }
        }
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        // Drop the audio thread.
        self.stop_chan.take();
        self.write_ws.shutdown().await?;
        Ok(())
    }
}
