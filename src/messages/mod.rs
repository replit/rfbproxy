//! Parsers for RFB messages.
#![allow(dead_code)]

pub mod client;
mod io;

use std::fmt::Display;

use bytes::Buf;

/// An error for the message parsers. `Incomplete` is used to signal that the buffer from where the
/// message is being parsed does not contain enough data to parse a full message.
#[derive(Debug)]
pub enum Error {
    Incomplete,
    Other(anyhow::Error),
}

impl std::error::Error for Error {}

impl Display for Error {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Incomplete => "stream ended early".fmt(fmt),
            Error::Other(err) => err.fmt(fmt),
        }
    }
}

/// A structure that represents how pixel values are represented in `FramebufferUpdate` messages.
///
/// This is documented at
/// https://github.com/rfbproto/rfbproto/blob/master/rfbproto.rst#setpixelformat
#[derive(Debug)]
pub struct PixelFormat {
    pub bits_per_pixel: u8,
    depth: u8,
    big_endian_flag: u8,
    true_colour_flag: u8,
    red_max: u16,
    green_max: u16,
    blue_max: u16,
    red_shift: u8,
    green_shift: u8,
    blue_shift: u8,
    padding: [u8; 3],
}

impl PixelFormat {
    pub fn new(buf: &[u8]) -> PixelFormat {
        let mut cur = std::io::Cursor::new(buf);
        PixelFormat {
            bits_per_pixel: cur.get_u8(),
            depth: cur.get_u8(),
            big_endian_flag: cur.get_u8(),
            true_colour_flag: cur.get_u8(),
            red_max: cur.get_u16(),
            green_max: cur.get_u16(),
            blue_max: cur.get_u16(),
            red_shift: cur.get_u8(),
            green_shift: cur.get_u8(),
            blue_shift: cur.get_u8(),
            padding: [0, 0, 0],
        }
    }
}
