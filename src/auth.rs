//! A wrapper for performing Remote Framebuffer authentication.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Context, Result};
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

/// Validate a Goval Handshake v5 token. It should be:
///
/// - Issued by one of the known public keys.
/// - Be valid at this point in time.
/// - Be issued for the repl where this is being run.
#[allow(dead_code)]
fn validate_token(token: &str, replid: &str, pubkeys: &HashMap<String, Vec<u8>>) -> Result<()> {
    use prost::Message;

    let token_parts = token.split('.').collect::<Vec<_>>();
    if token_parts.len() != 4 {
        bail!("token has wrong number of parts: {}", token_parts.len());
    }
    let raw_footer = base64::decode_config(token_parts[3], base64::URL_SAFE_NO_PAD)
        .context("failed to extract the PASETO footer")?;
    let footer = crate::api::GovalTokenMetadata::decode(
        &*base64::decode(&raw_footer).context("failed to base64-decode the PASETO footer")?,
    )
    .context("failed to parse the PASETO footer")?;

    let repl_token = crate::api::ReplToken::decode(
        &*base64::decode(&match paseto::v2::verify_paseto(
            &token,
            Some(&std::str::from_utf8(&raw_footer)?),
            pubkeys
                .get(&footer.key_id)
                .ok_or_else(|| anyhow!("could not find {} in pubkeys", &footer.key_id))?,
        ) {
            Ok(message) => message,
            Err(err) => bail!("failed to verify PASETO: {}", err),
        })
        .context("failed to base64-decode the PASETO message")?,
    )
    .context("failed to parse the PASETO message")?;

    // Validate issue / expiration timestamps.
    let iat = match repl_token.iat.as_ref() {
        Some(ts) => std::time::SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::new(ts.seconds as u64, ts.nanos as u32))
            .ok_or_else(|| anyhow!("overflow decoding iat: {:?}", repl_token.iat.as_ref()))?,
        None => std::time::SystemTime::UNIX_EPOCH,
    };
    let exp = match repl_token.exp.as_ref() {
        Some(ts) => std::time::SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::new(ts.seconds as u64, ts.nanos as u32))
            .ok_or_else(|| anyhow!("overflow decoding exp: {:?}", repl_token.exp.as_ref()))?,
        None => iat
            .checked_add(std::time::Duration::from_secs(3600))
            .ok_or_else(|| anyhow!("overflow providing fallback iat: {:?}", &iat))?,
    };
    let now = std::time::SystemTime::now();
    if now < iat {
        bail!(
            "token issued in the past: {}",
            chrono::DateTime::<chrono::offset::Utc>::from(iat).to_rfc3339()
        );
    }
    if now > exp {
        bail!(
            "token expired: {}",
            chrono::DateTime::<chrono::offset::Utc>::from(exp).to_rfc3339()
        );
    }

    // Validate ReplID.
    let token_replid = match &repl_token.metadata {
        Some(crate::api::repl_token::Metadata::Repl(repl)) => repl.id.clone(),
        Some(crate::api::repl_token::Metadata::Id(id)) => id.id.clone(),
        _ => bail!("token does not contain a replid: {:?}", &repl_token),
    };
    if token_replid != replid {
        bail!(
            "token not issued for replid {:?}: {:?}",
            &token_replid,
            replid,
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use bytes::BytesMut;
    use prost::Message;
    use ring::signature::KeyPair;
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

    #[test]
    fn test_validate_token() {
        init();

        let replid = "repl";

        let sys_rand = ring::rand::SystemRandom::new();

        let keyid = "keyid";
        let keypair = ring::signature::Ed25519KeyPair::from_pkcs8(
            ring::signature::Ed25519KeyPair::generate_pkcs8(&sys_rand)
                .expect("Failed to generate pkcs8 key!")
                .as_ref(),
        )
        .expect("Failed to parse keypair");
        let pubkey = keypair.public_key();
        let mut pubkeys = HashMap::<String, Vec<u8>>::new();
        pubkeys.insert(keyid.to_string(), pubkey.as_ref().to_vec());

        let keyid_other = "keyid_other";
        let keypair_other = ring::signature::Ed25519KeyPair::from_pkcs8(
            ring::signature::Ed25519KeyPair::generate_pkcs8(&sys_rand)
                .expect("Failed to generate pkcs8 key!")
                .as_ref(),
        )
        .expect("Failed to parse keypair");
        let pubkey_other = keypair_other.public_key();
        let mut pubkeys_other = HashMap::<String, Vec<u8>>::new();
        pubkeys_other.insert(keyid_other.to_string(), pubkey_other.as_ref().to_vec());

        let mut pubkeys_wrong_pubkey = HashMap::<String, Vec<u8>>::new();
        pubkeys_wrong_pubkey.insert(keyid.to_string(), pubkey_other.as_ref().to_vec());

        let token = mint_token(
            &replid,
            &keyid,
            None,
            Some(prost_types::Timestamp {
                seconds: 253402329599,
                nanos: 0,
            }),
            &keypair,
        )
        .expect("Failed to generate PASETO");

        validate_token(&token, &replid.to_string(), &pubkeys).expect("Failed to validate token");
        validate_token(
            &String::from("this is not a token"),
            &replid.to_string(),
            &pubkeys,
        )
        .expect_err("Should have rejected an invalid token");
        validate_token(&token, &replid.to_string(), &pubkeys_wrong_pubkey)
            .expect_err("Should have rejected a token signed with a mismatched key");
        validate_token(&token, &replid.to_string(), &pubkeys_other)
            .expect_err("Should have rejected a token signed with an unknown key");
        validate_token(&token, &String::from("other repl"), &pubkeys)
            .expect_err("Should have rejected a token signed for another repl");

        validate_token(
            &mint_token(
                &replid,
                &keyid,
                None,
                Some(prost_types::Timestamp {
                    seconds: 0,
                    nanos: 0,
                }),
                &keypair,
            )
            .expect("Failed to generate PASETO"),
            &replid.to_string(),
            &pubkeys,
        )
        .expect_err("Should have rejected an expired token");
        validate_token(
            &mint_token(&replid, &keyid, None, None, &keypair).expect("Failed to generate PASETO"),
            &replid.to_string(),
            &pubkeys,
        )
        .expect_err("Should have rejected an (implicitly) expired token");
        validate_token(
            &mint_token(
                &replid,
                &keyid,
                Some(prost_types::Timestamp {
                    seconds: 253402329599,
                    nanos: 0,
                }),
                Some(prost_types::Timestamp {
                    seconds: 253402329599,
                    nanos: 0,
                }),
                &keypair,
            )
            .expect("Failed to generate PASETO"),
            &replid.to_string(),
            &pubkeys,
        )
        .expect_err("Should have rejected a not-yet-issued token");
    }

    fn mint_token(
        replid: &str,
        keyid: &str,
        iat: Option<prost_types::Timestamp>,
        exp: Option<prost_types::Timestamp>,
        keypair: &ring::signature::Ed25519KeyPair,
    ) -> Result<String> {
        let mut buf = BytesMut::with_capacity(1024);

        let mut repl_token = crate::api::ReplToken::default();
        repl_token.iat = iat;
        repl_token.exp = exp;
        repl_token.cluster = String::from("development");
        repl_token.metadata = Some(crate::api::repl_token::Metadata::Id(
            crate::api::repl_token::ReplId {
                id: String::from(replid),
                source_repl: String::from(""),
            },
        ));
        repl_token.encode(&mut buf).expect("could not encode token");
        let message = base64::encode(&buf);

        buf.clear();
        crate::api::GovalTokenMetadata {
            key_id: String::from(keyid),
        }
        .encode(&mut buf)
        .expect("could not encode footer");
        let footer = base64::encode(&buf);

        let token = match paseto::v2::public_paseto(&message, Some(&footer), keypair) {
            Ok(token) => token,
            Err(err) => bail!("failed to generate PASETO: {}", err),
        };

        Ok(token)
    }
}
