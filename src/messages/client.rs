use super::io::*;
use super::Error;

use std::fmt::Display;

use anyhow::{anyhow, Result};
use bytes::Buf;

/// Represents a message that is sent from the client to the server.
#[derive(Debug)]
pub enum Message {
    SetPixelFormat(Vec<u8>),
    SetEncodings(Vec<u8>),
    FramebufferUpdateRequest(Vec<u8>),
    KeyEvent(Vec<u8>),
    PointerEvent(Vec<u8>),
    ClientCutText(Vec<u8>),

    // Extensions
    EnableContinuousUpdates(Vec<u8>),
    ClientFence(Vec<u8>),
    SetDesktopSize(Vec<u8>),
    QemuClientMessage(Vec<u8>),
    ReplitClientAudioStartEncoder(Vec<u8>, bool, u8, u16, u16),
    ReplitClientAudioFrameRequest(Vec<u8>),
    ReplitClientAudioStartContinuousUpdates(Vec<u8>),
}

impl Display for Message {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        use Message::*;

        match self {
            SetEncodings(payload) => {
                let mut cur = std::io::Cursor::new(&payload[4..]);
                write!(fmt, "SetEncodings(")?;
                while cur.has_remaining() {
                    write!(fmt, " {}", cur.get_i32())?;
                }
                write!(fmt, " )")
            }
            m => {
                write!(fmt, "{:?}", m)
            }
        }
    }
}

impl Message {
    /// Gets the raw u8 array representation of the message.
    pub fn into_data(self) -> Vec<u8> {
        use Message::*;

        match self {
            SetPixelFormat(payload)
            | SetEncodings(payload)
            | FramebufferUpdateRequest(payload)
            | KeyEvent(payload)
            | PointerEvent(payload)
            | ClientCutText(payload)
            | EnableContinuousUpdates(payload)
            | ClientFence(payload)
            | SetDesktopSize(payload)
            | ReplitClientAudioStartEncoder(payload, _, _, _, _)
            | ReplitClientAudioFrameRequest(payload)
            | ReplitClientAudioStartContinuousUpdates(payload)
            | QemuClientMessage(payload) => payload,
        }
    }

    /// Gets the length of the raw u8 array representation of the message.
    pub fn len(&self) -> usize {
        use Message::*;

        match self {
            SetPixelFormat(payload)
            | SetEncodings(payload)
            | FramebufferUpdateRequest(payload)
            | KeyEvent(payload)
            | PointerEvent(payload)
            | ClientCutText(payload)
            | EnableContinuousUpdates(payload)
            | ClientFence(payload)
            | SetDesktopSize(payload)
            | ReplitClientAudioStartEncoder(payload, _, _, _, _)
            | ReplitClientAudioFrameRequest(payload)
            | ReplitClientAudioStartContinuousUpdates(payload)
            | QemuClientMessage(payload) => payload.len(),
        }
    }

    /// Tries to parse a message from a [`std::io::Cursor`].
    ///
    /// # Errors
    ///
    /// IF there is not enough data in `src` to parse a complete message, it will return a
    /// [`Error::Incomplete`].
    pub(crate) fn parse(src: &mut std::io::Cursor<&[u8]>) -> Result<Message, Error> {
        match peek_u8(src)? {
            0 => {
                let payload = read(src, 20)?;
                Ok(Message::SetPixelFormat(payload.to_vec()))
            }
            2 => {
                skip(src, 2)?;
                let number_of_encodings = get_u16(src)? as usize;
                src.set_position(0);
                let payload = read(src, 4 + 4 * number_of_encodings)?;
                Ok(Message::SetEncodings(payload.to_vec()))
            }
            3 => {
                let payload = read(src, 10)?;
                Ok(Message::FramebufferUpdateRequest(payload.to_vec()))
            }
            4 => {
                let payload = read(src, 8)?;
                Ok(Message::KeyEvent(payload.to_vec()))
            }
            5 => {
                let payload = read(src, 6)?;
                Ok(Message::PointerEvent(payload.to_vec()))
            }
            6 => {
                skip(src, 4)?; // id + padding
                let length = get_i32(src)?;

                if length >= 0 {
                    // Standard message
                    check_message_length(length as usize)?;
                    let len = src.position() as usize;
                    src.set_position(0);
                    let payload = read(src, len + length as usize)?;
                    return Ok(Message::ClientCutText(payload.to_vec()));
                }

                // Extended message
                check_message_length(-length as usize)?;
                let flags = get_u32(src)?;
                log::debug!("Extended clipboard message: {} {:x}", -length, flags);

                src.set_position(0);
                let payload = read(src, 8 + (-length) as usize)?;
                Ok(Message::ClientCutText(payload.to_vec()))
            }
            150 => {
                let payload = read(src, 10)?;
                Ok(Message::EnableContinuousUpdates(payload.to_vec()))
            }
            245 => {
                // Replit Audio Client Message
                skip(src, 1)?; // id
                let submessage_id = get_u8(src)?;
                let length = get_u16(src)? as usize;
                match submessage_id {
                    0 => {
                        // Replit Audio Client Message StartEncoder
                        if length != 6 {
                            return Err(Error::Other(anyhow!(
                                "unexpected StartEncoder length: got {}, expected 6",
                                length
                            )));
                        }

                        let enabled = get_u8(src)? != 0;
                        let channels = get_u8(src)?;
                        let codec = get_u16(src)?;
                        let kbps = get_u16(src)?;
                        src.set_position(0);
                        let payload = read(src, length + 4)?;
                        Ok(Message::ReplitClientAudioStartEncoder(
                            payload.to_vec(),
                            enabled,
                            channels,
                            codec,
                            kbps,
                        ))
                    }
                    1 => {
                        // Replit Audio Client Message FrameRequest
                        if length != 0 {
                            return Err(Error::Other(anyhow!(
                                "unexpected FrameRequest length: got {}, expected 0",
                                length
                            )));
                        }

                        src.set_position(0);
                        let payload = read(src, length + 4)?;
                        Ok(Message::ReplitClientAudioFrameRequest(payload.to_vec()))
                    }
                    2 => {
                        // Replit Audio Client Message EnableContinuousUpdates
                        if length != 0 {
                            return Err(Error::Other(anyhow!(
                                "unexpected EnableContinuousUpdates length: got {}, expected 0",
                                length
                            )));
                        }

                        src.set_position(0);
                        let payload = read(src, length + 4)?;
                        Ok(Message::ReplitClientAudioStartContinuousUpdates(
                            payload.to_vec(),
                        ))
                    }
                    submessage_id => {
                        src.set_position(0);
                        Err(Error::Other(anyhow!(
                            "unsupported Replit Client submessage id: {} {:?}",
                            submessage_id,
                            src.get_ref()
                        )))
                    }
                }
            }
            248 => {
                skip(src, 8)?; // id, padding, flags
                let length = get_u8(src)? as usize;

                let len = src.position() as usize;
                src.set_position(0);
                let payload = read(src, len + length)?;
                Ok(Message::ClientFence(payload.to_vec()))
            }
            251 => {
                skip(src, 6)?; // id, padding, width, height
                let number_of_screens = get_u8(src)? as usize;

                let len = src.position() as usize;
                src.set_position(0);
                let payload = read(src, len + 1 + 16 * number_of_screens)?;
                Ok(Message::SetDesktopSize(payload.to_vec()))
            }
            255 => {
                skip(src, 1)?; // id
                match get_u8(src)? {
                    0 => {
                        // QEMU Extended Key Event Message.
                        src.set_position(0);
                        let payload = read(src, 12)?;
                        Ok(Message::QemuClientMessage(payload.to_vec()))
                    }
                    submessage_id => {
                        src.set_position(0);
                        Err(Error::Other(anyhow!(
                            "unsupported QEMU Client submessage id: {} {:?}",
                            submessage_id,
                            src.get_ref()
                        )))
                    }
                }
            }
            message_id => Err(Error::Other(anyhow!(
                "unsupported client message id: {} {:?}",
                message_id,
                src.get_ref(),
            ))),
        }
    }
}
