//! A stream encoder that gets the audio samples from PulseAudio.
//!
//! Currently, Opus (in a WebM streamable container) and MP3 (in an MPEG-1 container) are
//! supported, which should cover all of the major browsers.

mod mp3;
mod opus;

use anyhow::{bail, Context, Result};
use byte_slice_cast::*;
use psimple;
use pulse::sample;
use pulse::stream::Direction;
use tokio::sync::{mpsc, oneshot};

/// A muxer that can write frames of encoded audio into a container that is appropriate for
/// streaming.
trait StreamMuxer {
    /// Writes an encoded packet into a [`std::io::Write`].
    fn write_frame<W: std::io::Write>(
        &mut self,
        w: &mut W,
        frame: &[u8],
        timestamp: u64,
        keyframe: bool,
    ) -> Result<()>;
}

/// An encoder for an audio stream.
trait Encoder {
    /// The preferred sample rate for this encoder.
    fn sample_rate(&self) -> u32;

    /// The frame length, in milliseconds.
    fn frame_len_ms(&self) -> u64;

    /// The number of channels.
    fn channels(&self) -> u8 {
        2
    }

    /// The internal delay of the codec, in samples.
    fn internal_delay(&mut self) -> Result<usize>;

    /// The maximum size of an encoded and contained frame.
    fn max_frame_len(&self) -> usize;

    /// Encodes a frame into the provided buffer, including the container. Returns the size of the
    /// payload, and whether the frame is a keyframe.
    fn encode_frame(
        &mut self,
        timestamp: u64,
        input: &[i16],
        output: &mut [u8],
    ) -> Result<(usize, bool)>;
}

/// A stream that encodes PulseAudio-provided audio and generates Replit Audio Server AudioData
/// messages.
pub struct Stream {
    enc: Box<dyn Encoder + Send>,
    pulse: psimple::Simple,
    buffer: Vec<i16>,
    stream_start: Option<std::time::Instant>,
    timestamp: u64,
    dropped_frames: u64,
}

impl Stream {
    pub fn new(channels: u8, codec: u16, kbps: u16) -> Result<Stream> {
        let enc: Box<dyn Encoder + Send> = match codec {
            0 => Box::new(
                opus::OpusEncoder::new(channels, kbps).context("could not create Opus encoder")?,
            ),
            1 => Box::new(
                mp3::Mp3Encoder::new(channels, kbps).context("could not create MP3 encoder")?,
            ),
            _ => bail!("unsupported codec: {}", codec),
        };
        let frame_len_ms = enc.frame_len_ms();
        let sample_rate = enc.sample_rate();

        log::debug!("Opened audio pipe");

        let spec = sample::Spec {
            format: sample::Format::S16le,
            channels: channels,
            rate: enc.sample_rate(),
        };
        if !spec.is_valid() {
            bail!("invalid channels / sample rate combination");
        }

        let pulse = match psimple::Simple::new(
            None,              // Use the default server
            "audiomuxer",      // Our application’s name
            Direction::Record, // We want a recording stream
            None,              // Use the default device
            "Music",           // Description of our stream
            &spec,             // Our sample format
            None,              // Use default channel map
            None,              // Use default buffering attributes
        ) {
            Ok(pulse) => pulse,
            Err(e) => bail!(e),
        };

        log::debug!("Opened audio pipe");

        Ok(Stream {
            enc: enc,
            pulse: pulse,
            buffer: vec![
                0i16;
                channels as usize * sample_rate as usize * frame_len_ms as usize / 1000
            ],
            stream_start: None,
            dropped_frames: 0,
            timestamp: -(frame_len_ms as i64) as u64,
        })
    }

    /// Reads a single audio frame into the internal buffer.
    fn read_frame(&mut self) -> Result<()> {
        if let Err(e) = self.pulse.read(self.buffer.as_mut_byte_slice()) {
            bail!("failed to read PulseAudio data: {}", e);
        }
        self.timestamp = std::cmp::max(
            self.timestamp + self.enc.frame_len_ms(),
            self.stream_start
                .get_or_insert_with(|| std::time::Instant::now())
                .elapsed()
                .as_millis() as u64,
        );
        log::debug!("Read audio chunk");
        Ok(())
    }

    /// Encodes the frame from the internal buffer and returns the Replit AudioFrame contents.
    fn encode_frame(&mut self) -> Result<Vec<u8>> {
        let mut payload = Vec::<u8>::with_capacity(self.enc.max_frame_len());
        payload.resize(self.enc.max_frame_len(), 0);
        payload[..2].copy_from_slice(&[
            0xF5, // message-type
            0x01, // submessage-type
        ]);

        let (mut payload_size, keyframe) = self
            .enc
            .encode_frame(self.timestamp, &self.buffer, &mut payload[8..])
            .context("could not encode frame")?;
        payload_size += 4;

        if payload_size > 0xFFFF {
            bail!("payload exceeds maximum size: {}", payload_size);
        }

        // The Most Significant Bit marks whether the frame is a keyframe.
        let timestamp = (self.timestamp & 0x7FFFFFFF) | ((keyframe as u64) << 31);
        payload[2] = ((payload_size >> 8) & 0xff) as u8;
        payload[3] = ((payload_size >> 0) & 0xff) as u8;
        payload[4] = ((timestamp >> 24) & 0xff) as u8;
        payload[5] = ((timestamp >> 16) & 0xff) as u8;
        payload[6] = ((timestamp >> 8) & 0xff) as u8;
        payload[7] = ((timestamp >> 0) & 0xff) as u8;
        payload.truncate(payload_size + 8);

        log::debug!(
            "Sending {} payload bytes at timestamp {}, {} frames dropped",
            payload.len(),
            self.timestamp,
            self.dropped_frames,
        );

        Ok(payload)
    }

    /// Runs a PulseAudio thread using the simple (blocking) API.
    pub fn run(
        mut self,
        stop_chan: oneshot::Sender<()>,
        audio_message_chan: mpsc::Sender<Vec<u8>>,
    ) -> () {
        log::info!("audio thread started");

        while !stop_chan.is_closed() {
            // Always consume the frame from PulseAudio. That way this thread doesn't end up with
            // huge jumps or large number of dropped messages.
            if let Err(e) = self.read_frame() {
                log::error!("failed to read audio frame: {}", e);
                break;
            }
            let permit = match audio_message_chan.try_reserve() {
                Ok(permit) => permit,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    log::debug!("Dropped message due to backpressure");
                    self.dropped_frames += 1;
                    continue;
                }
                Err(e) => {
                    log::error!("failed to send audio data: {}", e);
                    break;
                }
            };
            let payload = match self.encode_frame() {
                Ok(payload) => payload,
                Err(e) => {
                    log::error!("failed to read audio frame: {}", e);
                    break;
                }
            };
            permit.send(payload);
        }
        log::info!(
            "audio thread finished, {} dropped frames",
            self.dropped_frames
        );
    }
}
