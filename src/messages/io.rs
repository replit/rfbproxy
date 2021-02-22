use super::Error;

use anyhow::{anyhow, Result};
use bytes::Buf;

/// The maximum message length. Anything larger than this will be rejected.
pub(crate) const MAX_MESSAGE_LENGTH: usize = 4 * 1024 * 1024;

pub(crate) fn check_message_length<T>(length: T) -> Result<(), Error>
where
    T: Into<usize>,
{
    let message_length = length.into();
    if message_length > MAX_MESSAGE_LENGTH {
        return Err(Error::Other(anyhow!(
            "message size is larger than the allowed limit: {}",
            message_length,
        )));
    }
    Ok(())
}

pub(crate) fn peek_u8(src: &mut std::io::Cursor<&[u8]>) -> Result<u8, Error> {
    if !src.has_remaining() {
        return Err(Error::Incomplete);
    }

    Ok(src.chunk()[0])
}

pub(crate) fn get_u8(src: &mut std::io::Cursor<&[u8]>) -> Result<u8, Error> {
    if !src.has_remaining() {
        return Err(Error::Incomplete);
    }

    Ok(src.get_u8())
}

pub fn get_u16(src: &mut std::io::Cursor<&[u8]>) -> Result<u16, Error> {
    if src.remaining() < 2 {
        return Err(Error::Incomplete);
    }

    Ok(src.get_u16())
}

pub fn get_u32(src: &mut std::io::Cursor<&[u8]>) -> Result<u32, Error> {
    if src.remaining() < 4 {
        return Err(Error::Incomplete);
    }

    Ok(src.get_u32())
}

pub fn get_i32(src: &mut std::io::Cursor<&[u8]>) -> Result<i32, Error> {
    if src.remaining() < 4 {
        return Err(Error::Incomplete);
    }

    Ok(src.get_i32())
}

pub fn get_compact(src: &mut std::io::Cursor<&[u8]>) -> Result<usize, Error> {
    let mut l = get_u8(src)? as usize;
    let mut len = l & 0x7f;
    if (l & 0x80) != 0 {
        l = get_u8(src)? as usize;
        len |= (l & 0x7f) << 7;
        if (l & 0x80) != 0 {
            l = get_u8(src)? as usize;
            len |= l << 14;
        }
    }

    Ok(len)
}

pub fn skip(src: &mut std::io::Cursor<&[u8]>, n: usize) -> Result<(), Error> {
    if src.remaining() < n {
        return Err(Error::Incomplete);
    }

    src.advance(n);
    Ok(())
}

pub fn read<'a>(src: &mut std::io::Cursor<&'a [u8]>, n: usize) -> Result<&'a [u8], Error> {
    if src.remaining() < n {
        return Err(Error::Incomplete);
    }

    Ok(&src.get_ref()[0..n])
}

pub fn peek(src: &std::io::Cursor<&[u8]>, n: usize) -> Result<Vec<u8>, Error> {
    if src.remaining() < n {
        return Err(Error::Incomplete);
    }

    Ok(src.chunk()[0..n].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_compact() -> Result<(), Error> {
        let data = vec![0x90, 0x4E];
        let mut cur = std::io::Cursor::new(&data[..]);
        assert_eq!(get_compact(&mut cur)?, 10000);
        Ok(())
    }
}
