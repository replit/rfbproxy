//! An RFB proxy that enables WebSockets.
//!
//! This crate proxies a TCP Remote Framebuffer server connection and exposes a WebSocket endpoint,
//! translating the connection between them.

extern crate anyhow;
extern crate clap;
extern crate env_logger;
extern crate futures;
extern crate hyper;
extern crate hyper_staticfile;
extern crate hyper_tungstenite;
extern crate log;
extern crate path_clean;
extern crate tokio;

use std::net::SocketAddr;

use anyhow::{anyhow, Result};

use futures::{SinkExt, StreamExt};

use hyper::{Body, Request, Response, Server};

use path_clean::PathClean;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::protocol::Message;

mod audio;
mod messages;

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
        loop {
            match rws.next().await {
                Some(msg) => {
                    if let Ok(Message::Binary(payload)) = msg {
                        if let Err(err) = ws.write_all(&payload).await {
                            log::error!("failed to write a message to the server: {}", err);
                            break;
                        }
                    }
                }
                None => {
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

/// Handles a single WebSocket connection.
async fn handle_connection<Stream>(
    rfb_addr: std::net::SocketAddr,
    ws_stream: tokio_tungstenite::WebSocketStream<Stream>,
) -> Result<()>
where
    Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let socket = TcpStream::connect(rfb_addr).await?;

    forward_streams(socket, ws_stream).await
}

/// Handles HTTP requests. Will serve any files in the current working directory, and the RFB
/// websocket in `/ws`.
async fn handle_request(
    rfb_addr: std::net::SocketAddr,
    mut req: Request<Body>,
    remote_addr: SocketAddr,
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
            if let Err(e) = handle_connection(rfb_addr, ws_stream).await {
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

    let matches = clap::App::new("rfbproxy")
        .about("An RFB proxy that enables WebSockets")
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
        .get_matches();

    // Create the event loop and TCP listener we'll accept connections on.
    let local_addr = matches
        .value_of("address")
        .ok_or(anyhow!("missing --address arg"))?;
    let rfb_addr: std::net::SocketAddr = matches
        .value_of("rfb-server")
        .ok_or(anyhow!("missing --rfb-server arg"))?
        .parse()?;
    if matches.is_present("http-server") {
        let server = Server::bind(&local_addr.parse()?).serve(hyper::service::make_service_fn(
            |conn: &hyper::server::conn::AddrStream| {
                let remote_addr = conn.remote_addr();
                async move {
                    Ok::<_, hyper::Error>(hyper::service::service_fn(
                        move |req: Request<Body>| async move {
                            handle_request(rfb_addr, req, remote_addr).await
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
                if let Err(e) = handle_connection(rfb_addr, ws_stream).await {
                    log::error!("error in websocket connection: {}", e);
                }
                log::info!("{} disconnected", remote_addr);
            });
        }
    }

    Ok(())
}
