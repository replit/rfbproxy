use super::io::*;
use super::Error;

use anyhow::{anyhow, Result};

/// Represents a message that is sent from the server to the client.
#[derive(Debug)]
#[allow(clippy::enum_variant_names)]
pub enum Message {
    FramebufferUpdate(Vec<u8>),
    SetColorMapEntries(Vec<u8>),
    Bell(Vec<u8>),
    ServerCutText(Vec<u8>),

    // Extensions
    EndOfContinuousUpdates(Vec<u8>),
    ServerFence(Vec<u8>),
    ReplitAudioServerMessage(Vec<u8>),
}

impl Message {
    /// Gets the raw u8 array representation of the message.
    pub fn into_data(self) -> Vec<u8> {
        use Message::*;

        match self {
            FramebufferUpdate(payload)
            | SetColorMapEntries(payload)
            | Bell(payload)
            | ServerCutText(payload)
            | EndOfContinuousUpdates(payload)
            | ReplitAudioServerMessage(payload)
            | ServerFence(payload) => payload,
        }
    }

    /// Gets the length of the raw u8 array representation of the message.
    pub fn len(&self) -> usize {
        use Message::*;

        match self {
            FramebufferUpdate(payload)
            | SetColorMapEntries(payload)
            | Bell(payload)
            | ServerCutText(payload)
            | EndOfContinuousUpdates(payload)
            | ReplitAudioServerMessage(payload)
            | ServerFence(payload) => payload.len(),
        }
    }

    /// Tries to parse a message from a [`std::io::Cursor`].
    ///
    /// # Errors
    ///
    /// IF there is not enough data in `src` to parse a complete message, it will return a
    /// [`Error::Incomplete`].
    pub(crate) fn parse(
        src: &mut std::io::Cursor<&[u8]>,
        bytes_per_pixel: usize,
    ) -> Result<Message, Error> {
        match peek_u8(src)? {
            0 => {
                skip(src, 2)?; // id + padding
                let number_of_rectangles = get_u16(src)?;
                for _ in 0..number_of_rectangles {
                    let _x = get_u16(src)? as usize;
                    let _y = get_u16(src)? as usize;
                    let width = get_u16(src)? as usize;
                    let height = get_u16(src)? as usize;

                    log::debug!("Rectangle encoding {:?}", peek(src, 4)?);

                    // encoding type
                    (match get_i32(src)? {
                        // Raw Encoding, width*height*bytesPerPixel
                        0 => skip(src, width * height * bytes_per_pixel),
                        // CopyRect Encoding, 4 bytes
                        1 => skip(src, 4),
                        // Tight Encoding.
                        7 => {
                            let mut ctl = get_u8(src)?;
                            log::debug!("   Tight control {:x}", ctl);
                            ctl >>= 4;
                            if ctl == 0x08 {
                                skip(src, 3)
                            } else if ctl == 0x09 {
                                let len = get_compact(src)?;
                                skip(src, len)
                            } else {
                                let mut filter = 0u8;
                                if (ctl & 0x4) != 0 {
                                    filter = get_u8(src)?;
                                }
                                log::debug!("        Filter {:x}", filter);

                                if filter == 0 {
                                    // CopyFilter
                                    let uncompressed_size = width * height * 3;
                                    if uncompressed_size < 12 {
                                        skip(src, uncompressed_size)
                                    } else {
                                        let len = get_compact(src)?;
                                        skip(src, len)
                                    }
                                } else if filter == 1 {
                                    // PaletteFilter
                                    let number_of_colors = get_u8(src)? as usize + 1;

                                    log::debug!(
                                        "       number of colors: {}, {:?}",
                                        number_of_colors,
                                        peek(src, 3 * number_of_colors)?,
                                    );
                                    skip(src, 3 * number_of_colors)?;

                                    let bpp = if number_of_colors <= 2 { 1 } else { 8 };
                                    let row_size = (width * bpp + 7) / 8;
                                    let uncompressed_size = row_size * height;

                                    log::debug!(
                                        "       bpp: {}, row_size: {}, uncompressed_size: {}",
                                        bpp,
                                        row_size,
                                        uncompressed_size
                                    );

                                    if uncompressed_size < 12 {
                                        skip(src, uncompressed_size)
                                    } else {
                                        let len = get_compact(src)?;
                                        skip(src, len)
                                    }
                                } else {
                                    Err(Error::Other(anyhow!(
                                        "unsupported Tight filter: {}",
                                        filter
                                    )))
                                }
                            }
                        }
                        // ZRLE Encoding, 4 bytes + payload
                        16 => {
                            let length = get_u32(src)? as usize;
                            check_message_length(length)?;
                            skip(src, length)
                        }
                        // LastRect Pseudo-encoding
                        -224 => break,
                        // Cursor Pseudo-encoding
                        -239 => {
                            skip(src, width * height * bytes_per_pixel)?;
                            let row_size = (width + 7) / 8;
                            skip(src, row_size * height)
                        }
                        // QEMU Extended Key Event Pseudo-encoding
                        -258 => Ok(()),
                        // ExtendedDesktopSize Pseudo-encoding
                        -308 => {
                            let number_of_screens = get_u8(src)? as usize;
                            skip(src, 3 + number_of_screens * 16)
                        }
                        // VMware Cursor Pseudo-encoding
                        0x574d5664 => match get_u8(src)? {
                            0 => skip(src, 1 + 2 * width * height * bytes_per_pixel),
                            1 => skip(src, 1 + width * height * 4),
                            cursor_type => Err(Error::Other(anyhow!(
                                "unsupported VMWare Cursor type: {}",
                                cursor_type
                            ))),
                        },
                        encoding_type => Err(Error::Other(anyhow!(
                            "unsupported encoding type: {}",
                            encoding_type
                        ))),
                    })?
                }
                let len = src.position() as usize;
                src.set_position(0);
                let payload = read(src, len)?;
                Ok(Message::FramebufferUpdate(payload.to_vec()))
            }
            1 => {
                skip(src, 4)?; // id, padding, first color
                let number_of_colors = get_u16(src)? as usize;

                let len = src.position() as usize;
                src.set_position(0);
                let payload = read(src, len + number_of_colors * 6)?;
                Ok(Message::SetColorMapEntries(payload.to_vec()))
            }
            2 => {
                let payload = read(src, 1)?;
                Ok(Message::Bell(payload.to_vec()))
            }
            3 => {
                skip(src, 4)?; // id + padding
                let length = get_i32(src)?;

                if length >= 0 {
                    // Standard messagee
                    check_message_length(length as usize)?;
                    let len = src.position() as usize;
                    src.set_position(0);
                    let payload = read(src, len + length as usize)?;
                    return Ok(Message::ServerCutText(payload.to_vec()));
                }

                // Extended message
                check_message_length((-length) as usize)?;
                let flags = get_u32(src)?;
                log::debug!("Extended clipboard message: {} {:x}", -length, flags);

                src.set_position(0);
                let payload = read(src, 8 + (-length) as usize)?;
                Ok(Message::ServerCutText(payload.to_vec()))
            }
            150 => {
                let payload = read(src, 1)?;
                Ok(Message::EndOfContinuousUpdates(payload.to_vec()))
            }
            248 => {
                skip(src, 8)?; // id, padding, flags
                let length = get_u8(src)? as usize;

                let len = src.position() as usize;
                src.set_position(0);
                let payload = read(src, len + length)?;
                Ok(Message::ServerFence(payload.to_vec()))
            }
            message_id => Err(Error::Other(anyhow!(
                "unsupported server message id: {}",
                message_id
            ))),
        }
    }
}
