//! An MP3 stream in a MPEG-1 container.

use super::Encoder;

use anyhow::{bail, Context, Result};

/// Error checker for lame_sys functions.
///
/// Returns an [`anyhow::Error`] with the stringified error if the result of the function call is negative,
/// and the result of the function as-is otherwise.
fn check_err(num: std::os::raw::c_int) -> Result<std::os::raw::c_int> {
    if num == lame_sys::lame_errorcodes_t::LAME_GENERICERROR as i32 {
        bail!("lame: generic error");
    } else if num == lame_sys::lame_errorcodes_t::LAME_NOMEM as i32 {
        bail!("lame: out of memory");
    } else if num == lame_sys::lame_errorcodes_t::LAME_BADBITRATE as i32 {
        bail!("lame: unsupported bitrate");
    } else if num == lame_sys::lame_errorcodes_t::LAME_BADSAMPFREQ as i32 {
        bail!("lame: unsupported sample rate");
    } else if num == lame_sys::lame_errorcodes_t::LAME_INTERNALERROR as i32 {
        bail!("lame: internal error");
    } else if num == lame_sys::lame_errorcodes_t::FRONTEND_READERROR as i32 {
        bail!("lame: frontend read error");
    } else if num == lame_sys::lame_errorcodes_t::FRONTEND_WRITEERROR as i32 {
        bail!("lame: frontend write error");
    } else if num == lame_sys::lame_errorcodes_t::FRONTEND_FILETOOLARGE as i32 {
        bail!("lame: frontend file too large");
    } else if num < 0 {
        bail!("lame: unknown error: {}", num);
    }
    Ok(num)
}

/// Mp3Encoder is a MPEG-1 Audio Layer II streaming muxer compliant codec that can be played in a
/// browser using Media Source Extensions.
pub struct Mp3Encoder {
    ctx: *mut lame_sys::lame_global_flags,
}

impl Mp3Encoder {
    pub fn new(channels: u8, kbps: u16) -> Result<Mp3Encoder> {
        let ctx = unsafe { lame_sys::lame_init() };
        if ctx.is_null() {
            bail!("could not initialize the lame library");
        }
        check_err(unsafe { lame_sys::lame_set_num_channels(ctx, channels as i32) })
            .with_context(|| format!("failed to set channels to {channels}"))?;
        check_err(unsafe { lame_sys::lame_set_brate(ctx, kbps as i32) })
            .with_context(|| format!("failed to set kbps to {kbps}"))?;
        check_err(unsafe { lame_sys::lame_set_quality(ctx, 7) })
            .context("failed to set quality to 7")?;
        check_err(unsafe { lame_sys::lame_init_params(ctx) })
            .context("failed to initialize the lame parameters")?;
        Ok(Mp3Encoder { ctx })
    }
}

impl Drop for Mp3Encoder {
    fn drop(&mut self) {
        unsafe { lame_sys::lame_close(self.ctx) };
        self.ctx = std::ptr::null_mut();
    }
}

impl Encoder for Mp3Encoder {
    fn sample_rate(&self) -> u32 {
        44100
    }

    fn frame_len_ms(&self) -> u64 {
        // The MP3 codec uses 576 or 1152 samples (depending on the flags used to initialize the
        // codec), but the sample rate is not cleanly divisible by that. The closest would be 20ms,
        // but that causes stuttering, so 40ms it is!
        40
    }

    fn internal_delay(&mut self) -> Result<usize> {
        Ok(576)
    }

    fn max_frame_len(&self) -> usize {
        (1.25 * self.frame_len_ms() as f64 * self.sample_rate() as f64) as usize + 7200
    }

    fn encode_frame(
        &mut self,
        _timestamp: u64,
        input: &[i16],
        output: &mut [u8],
    ) -> Result<(usize, bool)> {
        Ok((
            check_err(unsafe {
                lame_sys::lame_encode_buffer_interleaved(
                    self.ctx,
                    input.as_ptr() as *mut i16,
                    (input.len() / 2) as std::os::raw::c_int,
                    output.as_mut_ptr(),
                    output.len() as std::os::raw::c_int,
                )
            })? as usize,
            true, // All MP3 frames have a valid MPEG-1 header.
        ))
    }
}

unsafe impl Send for Mp3Encoder {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::Encoder;

    use rand::Rng;

    fn init() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    #[test]
    fn test_mp3() -> Result<()> {
        init();

        let mut enc = Mp3Encoder::new(2, 32 * 1024).expect("could not create MP3 encoder");

        let mut chunk =
            vec![
                0i16;
                enc.channels() as usize * enc.sample_rate() as usize * enc.frame_len_ms() as usize
                    / 1000
            ];
        rand::thread_rng()
            .try_fill(&mut chunk[..])
            .expect("could not generate random payload");

        log::info!("Read audio chunk");

        let mut payload = Vec::<u8>::with_capacity(enc.max_frame_len());
        payload.resize(enc.max_frame_len(), 0);
        payload[..4].copy_from_slice(&[
            0xFF, // message-type
            0x01, // submessage-type
            0x00, 0x02, // operation
        ]);

        log::info!("Encoding audio chunk...");
        let mut payload_size = enc
            .encode_frame(0, &chunk, &mut payload[8..])
            .expect("could not encode audio data")
            .0;
        payload_size += enc
            .encode_frame(0, &chunk, &mut payload[8 + payload_size..])
            .expect("could not encode audio data")
            .0;
        log::info!("Wrote {} audio bytes!", payload_size);

        assert!(payload_size > 0);

        payload[4] = ((payload_size >> 24) & 0xff) as u8;
        payload[5] = ((payload_size >> 16) & 0xff) as u8;
        payload[6] = ((payload_size >> 8) & 0xff) as u8;
        payload[7] = ((payload_size >> 0) & 0xff) as u8;
        payload.truncate(payload_size + 8);
        assert!(payload.len() > 5);

        Ok(())
    }
}
