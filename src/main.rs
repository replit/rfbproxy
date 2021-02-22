//! An RFB proxy that enables WebSockets and audio.
//!
//! This crate proxies a TCP Remote Framebuffer server connection and exposes a WebSocket endpoint,
//! translating the connection between them. It can optionally enable audio using the Replit Audio
//! messages if the `--enable-audio` flag is passed or the `VNC_ENABLE_EXPERIMENTAL_AUDIO`
//! environment variable is set to a non-empty value.

extern crate anyhow;
extern crate clap;
extern crate env_logger;
extern crate futures;
extern crate hyper;
extern crate hyper_staticfile;
extern crate hyper_tungstenite;
extern crate log;
extern crate opus;
extern crate path_clean;
extern crate tokio;

mod audio;
mod messages;
mod rfb;

use std::net::SocketAddr;

use anyhow::{anyhow, Context, Result};

use futures::{SinkExt, StreamExt};

use hyper::{Body, Request, Response, Server};

use path_clean::PathClean;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::protocol::Message;

/// Forwards the data between `socket` and `ws_stream`. Doesn't do anything with the bytes.
async fn forward_streams<Stream>(
    mut socket: TcpStream,
    ws_stream: tokio_tungstenite::WebSocketStream<Stream>,
) -> Result<()>
where
    Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut wws, mut rws) = ws_stream.split();
    let (mut rs, mut ws) = socket.split();

    let client_to_server = async move {
        while let Some(msg) = rws.next().await {
            if let Ok(Message::Binary(payload)) = msg {
                if let Err(err) = ws.write_all(&payload).await {
                    log::error!("failed to write a message to the server: {}", err);
                    break;
                }
            }
        }

        log::info!("client disconnected");
        ws.shutdown().await?;
        Ok::<(), anyhow::Error>(())
    };

    let server_to_client = async move {
        let mut buffer = [0u8; 4096];
        loop {
            match rs.read(&mut buffer[..]).await {
                Ok(0) => {
                    break;
                }
                Ok(n) => {
                    if let Err(err) = wws.send(Message::Binary((&buffer[..n]).to_vec())).await {
                        log::error!("failed to write a message to the client: {}", err);
                        break;
                    }
                }
                Err(err) => {
                    log::error!("failed to read a message from the server: {}", err);
                    break;
                }
            }
        }

        log::info!("server disconnected");
        wws.close().await?;
        Ok::<(), anyhow::Error>(())
    };

    let (cts, stc) = tokio::join!(client_to_server, server_to_client);
    cts?;
    stc?;
    Ok(())
}

/// Handles a single WebSocket connection. If `enable_audio` is false, it will just forward the
/// data between them. Otherwise, it will parse and interpret each RFB packet and inject audio
/// data.
async fn handle_connection<Stream>(
    rfb_addr: std::net::SocketAddr,
    mut ws_stream: tokio_tungstenite::WebSocketStream<Stream>,
    enable_audio: bool,
) -> Result<()>
where
    Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let socket = TcpStream::connect(rfb_addr).await?;

    if !enable_audio {
        return forward_streams(socket, ws_stream).await;
    }

    let (server_tx, mut server_rx) = mpsc::channel(2);
    let (client_tx, mut client_rx) = mpsc::channel(2);
    let mut conn = rfb::RfbConnection::new(socket, &mut ws_stream, server_tx, client_tx).await?;

    let (mut wws, mut rws) = ws_stream.split();
    let (mut rs, mut ws) = conn.split();

    let client_to_server = async {
        loop {
            let payload = tokio::select! {
                Some(payload) = server_rx.recv() => Some(payload),
                Some(msg) = rws.next() => {
                    match msg.context("failed to read client-to-server message")? {
                        Message::Binary(payload) => Some(payload),
                        Message::Close(_) => break,
                        msg => {
                            log::debug!("    ->: Received a message {:?}", msg);
                            None
                        }
                    }
                },
                else => break,
            };

            match payload {
                Some(payload) => {
                    if let Err(err) = ws.write_all(&payload).await {
                        log::error!("failed to write message: {}", err);
                        break;
                    }
                }
                None => {}
            }
        }

        log::info!("client disconnected");
        ws.shutdown().await?;
        Ok::<(), anyhow::Error>(())
    };

    let server_to_client = async {
        loop {
            let payload = tokio::select! {
                Some(payload) = client_rx.recv() => Some(payload),
                message = rs.read_server_message() => {
                    match message.context("failed to read server-to-client message")? {
                        None => break,
                        Some(msg) => {
                            log::debug!("<-: {:?}", &msg);
                            Some(msg.into_data())
                        }
                    }
                },
                else => break,
            };

            match payload {
                Some(payload) => wws.send(Message::Binary(payload)).await?,
                None => {}
            }
        }
        log::info!("server disconnected");
        wws.close().await?;
        Ok::<(), anyhow::Error>(())
    };

    let (cts, stc) = tokio::join!(client_to_server, server_to_client);
    cts?;
    stc?;

    Ok(())
}

/// Handles HTTP requests. Will serve any files in the current working directory, and the RFB
/// websocket in `/ws`.
async fn handle_request(
    rfb_addr: std::net::SocketAddr,
    mut req: Request<Body>,
    remote_addr: SocketAddr,
    enable_audio: bool,
) -> Result<Response<Body>> {
    // Clean the path so that it can't be used to access files outside the current working
    // directory.
    *req.uri_mut() = {
        let uri = req.uri();
        let clean_path = String::from(
            std::path::PathBuf::from(uri.path())
                .clean()
                .to_str()
                .ok_or(anyhow!("failed to clean path: {:?}", uri.path()))?,
        );

        let mut builder = http::uri::Builder::new();
        if let Some(scheme) = uri.scheme() {
            builder = builder.scheme(scheme.as_str());
        }
        if let Some(authority) = uri.authority() {
            builder = builder.authority(authority.as_str());
        }
        if let Some(query) = uri.query() {
            builder = builder.path_and_query(format!("{}?{}", clean_path, query));
        } else {
            builder = builder.path_and_query(clean_path);
        }
        builder.build()?
    };

    if req.uri().path() == "/ws" {
        if !hyper_tungstenite::is_upgrade_request(&req) {
            log::info!("Not an upgrade request");
            return Ok(http::response::Builder::new()
                .status(http::StatusCode::NOT_FOUND)
                .body(Body::empty())
                .expect("unable to build response"));
        }

        let (response, websocket) = hyper_tungstenite::upgrade(req, None)?;

        tokio::spawn(async move {
            log::info!("Incoming TCP connection from: {}", remote_addr);

            let ws_stream = match websocket.await {
                Ok(ws_stream) => ws_stream,
                Err(e) => {
                    log::error!("error in websocket upgrade: {}", e);
                    return;
                }
            };
            if let Err(e) = handle_connection(rfb_addr, ws_stream, enable_audio).await {
                log::error!("error in websocket connection: {}", e);
            }
            log::info!("{} disconnected", remote_addr);
        });
        return Ok(response);
    }

    Ok(hyper_staticfile::Static::new(std::path::Path::new("./"))
        .serve(req)
        .await?)
}

#[doc(hidden)]
#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    let matches = clap::App::new("audiomux")
        .about("An RFB proxy that enables WebSockets and audio")
        .arg(
            clap::Arg::with_name("address")
                .long("address")
                .value_name("HOST:PORT")
                .default_value("0.0.0.0:5900")
                .help("The hostname and port in which the server will bind")
                .takes_value(true),
        )
        .arg(
            clap::Arg::with_name("rfb-server")
                .long("rfb-server")
                .value_name("HOST:PORT")
                .default_value("127.0.0.1:5901")
                .help("The hostname and port where the original RFB server is listening")
                .takes_value(true),
        )
        .arg(
            clap::Arg::with_name("http-server")
                .long("http-server")
                .help(
                "Whether a normal HTTP server will start to serve the current directory's contents",
            ),
        )
        .arg(
            clap::Arg::with_name("enable-audio")
                .long("enable-audio")
                .help("Whether the muxer will support audio muxing or be a simple WebSocket proxy"),
        )
        .get_matches();

    // Create the event loop and TCP listener we'll accept connections on.
    let local_addr = matches
        .value_of("address")
        .ok_or(anyhow!("missing --address arg"))?;
    let rfb_addr: std::net::SocketAddr = matches
        .value_of("rfb-server")
        .ok_or(anyhow!("missing --rfb-server arg"))?
        .parse()?;
    let enable_audio = matches.is_present("enable-audio")
        || std::env::var("VNC_ENABLE_EXPERIMENTAL_AUDIO").unwrap_or(String::from("")) != "";
    if matches.is_present("http-server") {
        let server = Server::bind(&local_addr.parse()?).serve(hyper::service::make_service_fn(
            |conn: &hyper::server::conn::AddrStream| {
                let remote_addr = conn.remote_addr();
                async move {
                    Ok::<_, hyper::Error>(hyper::service::service_fn(
                        move |req: Request<Body>| async move {
                            handle_request(rfb_addr, req, remote_addr, enable_audio).await
                        },
                    ))
                }
            },
        ));
        log::info!("Listening on: {}", local_addr);

        server.await?;
    } else {
        let listener = TcpListener::bind(&local_addr).await?;
        log::info!("Listening on: {}", local_addr);

        while let Ok((raw_stream, remote_addr)) = listener.accept().await {
            let ws_stream = tokio_tungstenite::accept_hdr_async(
                raw_stream,
                |request: &tungstenite::handshake::server::Request,
                 mut response: tungstenite::handshake::server::Response| {
                    const PROTOCOL_HEADER: &str = "Sec-WebSocket-Protocol";
                    if let Some(val) = request.headers().get(PROTOCOL_HEADER) {
                        response.headers_mut().insert(PROTOCOL_HEADER, val.clone());
                    }
                    Ok(response)
                },
            )
            .await?;
            tokio::spawn(async move {
                log::info!("Incoming TCP connection from: {}", remote_addr);
                if let Err(e) = handle_connection(rfb_addr, ws_stream, enable_audio).await {
                    log::error!("error in websocket connection: {}", e);
                }
                log::info!("{} disconnected", remote_addr);
            });
        }
    }

    Ok(())
}
