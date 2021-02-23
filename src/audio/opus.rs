//! An Opus stream in a WebM streaming container.

use super::{Encoder, StreamMuxer};

use anyhow::{bail, Context, Result};

use rand::Rng;

/// OpusEncoder is a WebM streaming compliant codec using the [`WebmStreamMuxer`], that can be
/// played in a browser using Media Source Extensions.
pub struct OpusEncoder {
    enc: opus::Encoder,
    muxer: WebmStreamMuxer,
    wrote_initialization_segment: bool,
}

impl OpusEncoder {
    const SAMPLE_RATE: u32 = 48000;
    const FRAME_LEN_MS: u64 = 40;

    /// Creates a new instance of an OpusEncoder.
    pub fn new(channels: u8, kbps: u16) -> Result<OpusEncoder> {
        let mut enc = opus::Encoder::new(
            Self::SAMPLE_RATE,
            match channels {
                1 => opus::Channels::Mono,
                2 => opus::Channels::Stereo,
                _ => bail!("invalid channels: {}", channels),
            },
            opus::Application::LowDelay,
        )
        .context("could not create Opus encoder")?;
        enc.set_bitrate(opus::Bitrate::Bits(kbps as i32 * 8))
            .context("could not set Opus bitrate")?;

        let internal_delay = enc
            .get_lookahead()
            .context("could not get encoder internal delay")? as usize;

        Ok(OpusEncoder {
            enc,
            muxer: WebmStreamMuxer::new(Self::SAMPLE_RATE, channels, internal_delay),
            wrote_initialization_segment: false,
        })
    }
}

impl Encoder for OpusEncoder {
    fn sample_rate(&self) -> u32 {
        Self::SAMPLE_RATE
    }

    fn frame_len_ms(&self) -> u64 {
        Self::FRAME_LEN_MS
    }

    fn encode_frame(
        &mut self,
        timestamp: u64,
        input: &[i16],
        output: &mut [u8],
    ) -> Result<(usize, bool)> {
        let keyframe = !self.wrote_initialization_segment;
        if keyframe {
            self.wrote_initialization_segment = true;
        }
        let mut encoded_chunk = [0u8; 4000]; // Opus' recommended buffer size.
        let encoded_chunk_length = self
            .enc
            .encode(input, &mut encoded_chunk)
            .context("could not encode audio data")?;
        log::debug!("Encoded {} audio bytes", encoded_chunk_length);
        let mut inner_cur = std::io::Cursor::new(output);
        assert_eq!(inner_cur.position(), 0);
        log::debug!("Encoding audio chunk...");
        self.muxer
            .write_frame(
                &mut inner_cur,
                &encoded_chunk[..encoded_chunk_length],
                timestamp,
                keyframe,
            )
            .context("could not write audio frame")?;
        Ok((inner_cur.position() as usize, keyframe))
    }

    fn internal_delay(&mut self) -> Result<usize> {
        Ok(self.enc.get_lookahead()? as usize)
    }

    fn max_frame_len(&self) -> usize {
        4512
    }
}

/// WebmStreamMuxer is a WebM streaming muxer compliant with [mse-byte-stream-format-webm], that can be
/// played in a browser using Media Source Extensions.
///
/// References:
///
/// * [mse-byte-stream-format-webm] https://www.w3.org/TR/mse-byte-stream-format-webm/
/// * https://www.webmproject.org/docs/container/
/// * https://www.matroska.org/technical/elements.html
/// * https://www.matroska.org/technical/codec_specs.html
/// * https://www.matroska.org/technical/basics.html
#[derive(Debug)]
pub struct WebmStreamMuxer {
    sample_rate: u32,
    channels: u8,
    internal_delay: usize,
}

impl WebmStreamMuxer {
    pub fn new(sample_rate: u32, channels: u8, internal_delay: usize) -> WebmStreamMuxer {
        WebmStreamMuxer {
            sample_rate,
            channels,
            internal_delay,
        }
    }

    /// Writes the WebM Initialization Segment, which consists of:
    ///
    /// - The EBML Header
    /// - The Segment header (with an unknown size)
    /// - The SeekHead element (to comply with the WebM spec and signal the lack of a Cues element)
    /// - The Segment Information element
    /// - The Tracks element
    ///
    /// This Initialization Segment can be sent once at the beginning of every connection.
    pub fn write_initialization_segment<W: std::io::Write>(&self, w: &mut W) -> Result<()> {
        let mut track_uid = [0u8; 7];
        rand::thread_rng()
            .try_fill(&mut track_uid)
            .context("could not generate random payload")?;

        let application_name = "audiomux-v0.0.0".as_bytes();
        assert_eq!(application_name.len(), 15);

        w.write_all(&[
            0x1A, 0x45, 0xDF, 0xA3, 0x9F, 0x42, 0x86, 0x81, 0x01, 0x42, 0xF7, 0x81, 0x01, 0x42,
            0xF2, 0x81, 0x04, 0x42, 0xF3, 0x81, 0x08, 0x42, 0x82, 0x84, 0x77, 0x65, 0x62, 0x6D,
            0x42, 0x87, 0x81, 0x04, 0x42, 0x85, 0x81, 0x02,
        ])?;

        // Segment header, size unknown.
        w.write_all(&[
            0x18, 0x53, 0x80, 0x67, // SegmentHeader
            0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // size = -1
        ])?;

        // SeekHead
        w.write_all(&[
            0x11, 0x4D, 0x9B, 0x74, // SeekHead
            0xAA, // size = 42
            0x4D, 0xBB, // Seek
            0x8B, // size = 11
            0x53, 0xAB, // SeekID
            0x84, // size = 4
            0x15, 0x49, 0xA9, 0x66, // KaxInfo
            0x53, 0xAC, // SeekPosition
            0x81, // size = 1
            0x2F, // 47
            0x4D, 0xBB, // Seek
            0x8B, // size = 11
            0x53, 0xAB, // SeekID
            0x84, // size = 4
            0x16, 0x54, 0xAE, 0x6B, // KaxTracks
            0x53, 0xAC, // SeekPosition
            0x81, // size = 1
            0x5F, // 95
            0x4D, 0xBB, // Seek
            0x8B, // size = 11
            0x53, 0xAB, // SeekID
            0x84, // size = 4
            0x1F, 0x43, 0xB6, 0x75, // KaxCluster
            0x53, 0xAC, // SeekID
            0x81, // size = 1
            0xA5, // 165
        ])?;

        // Segment Information
        w.write_all(&[
            0x15,
            0x49,
            0xA9,
            0x66, // Info
            0xAB, // size
            0x2A,
            0xD7,
            0xB1, // TimestampScale
            0x83, // size = 3
            0x0F,
            0x42,
            0x40, // 1_000_000
            0x4D,
            0x80,                                // Multiplexing app
            0x80 | application_name.len() as u8, // size
        ])?;
        w.write_all(&application_name)?;
        w.write_all(&[
            0x57,
            0x41,                                // Writing app
            0x80 | application_name.len() as u8, // size
        ])?;
        w.write_all(&application_name)?;

        // Tracks
        w.write_all(&[
            0x16,
            0x54,
            0xAE,
            0x6B, // Tracks
            0xC2, // Size = 66
            0xAE, // TrackEntry
            0xC0, // Size = 64
            0xD7, // TrackNumber
            0x81, // Size = 1
            0x01, // Track = 1
            0x73,
            0xC5,                         // TrackUID
            0x80 | track_uid.len() as u8, // Size
        ])?;
        w.write_all(&track_uid)?;
        let codec_delay_ns = self.internal_delay * 1_000_000_000 / self.sample_rate as usize;
        w.write_all(&[
            0x83, // TrackType
            0x81, // Size = 1
            0x02, // Audio
            0x86, // CodecID
            0x86, // Size = 6
            0x41,
            0x5F,
            0x4F,
            0x50,
            0x55,
            0x53, // "A_OPUS"
            0x56,
            0xAA, // CodecDelay
            0x84, //  Size = 4
            ((codec_delay_ns >> 24) & 0xff) as u8,
            ((codec_delay_ns >> 16) & 0xff) as u8,
            ((codec_delay_ns >> 8) & 0xff) as u8,
            (codec_delay_ns & 0xff) as u8,
            0x63,
            0xA2, // CodecPrivate
            0x93, // Size = 19
            0x4F,
            0x70,
            0x75,
            0x73,
            0x48,
            0x65,
            0x61,
            0x64, // "OpusHead"
            0x01, // version
            self.channels,
            (self.internal_delay & 0xff) as u8,
            ((self.internal_delay >> 8) & 0xff) as u8,
            (self.sample_rate & 0xff) as u8,
            ((self.sample_rate >> 8) & 0xff) as u8,
            ((self.sample_rate >> 16) & 0xff) as u8,
            ((self.sample_rate >> 24) & 0xff) as u8,
            0x00,
            0x00, // 0 output gain, le16s
            0x00, // Channel Mapping Family 8u
            0xE1, // Audio
            0x89, // Size = 9
            0xB5, // SamplingFrequency
            0x84, // Size = 4
            0x47,
            0x3B,
            0x80,
            0x00,          // IEEE-745: 48000.0f
            0x9F,          // Channels
            0x81,          // Size = 1
            self.channels, // Channels = 2
        ])?;

        Ok(())
    }
}

impl StreamMuxer for WebmStreamMuxer {
    /// Writes the an Opus-encoded packet in a Cluster. Ideally, we could use a ShortBlock instead
    /// of a whole Cluster, but this makes things a bit easier for the time being.
    fn write_frame<W: std::io::Write>(
        &mut self,
        w: &mut W,
        frame: &[u8],
        timestamp: u64,
        keyframe: bool,
    ) -> Result<()> {
        if keyframe {
            self.write_initialization_segment(w)?;
        }

        let cluster_len = frame.len() + 17;
        let simple_block_len = frame.len() + 4;
        // Cluster
        w.write_all(&[
            0x1F,
            0x43,
            0xB6,
            0x75, // Cluster
            0x40 | ((cluster_len) >> 8) as u8,
            (cluster_len & 0xff) as u8, // Size
            0xE7,                       // Timestamp
            0x88,                       // Size = 8
            ((timestamp >> 56) & 0xFF) as u8,
            ((timestamp >> 48) & 0xFF) as u8,
            ((timestamp >> 40) & 0xFF) as u8,
            ((timestamp >> 32) & 0xFF) as u8,
            ((timestamp >> 24) & 0xFF) as u8,
            ((timestamp >> 16) & 0xFF) as u8,
            ((timestamp >> 8) & 0xFF) as u8,
            (timestamp & 0xFF) as u8,
            0xA3, // SimpleBlock
            0x40 | ((simple_block_len) >> 8) as u8,
            (simple_block_len & 0xff) as u8, // size
            // SimpleBlock structure
            0x81, // Track = 1
            0x00,
            0x00, // timecode = 0x0000
            0x80,
        ])?;
        w.write_all(frame)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::Encoder;

    use std::io::Write;

    use matroska;
    use tempfile;

    fn init() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    #[test]
    fn test_webm() -> Result<()> {
        init();

        let mut enc = OpusEncoder::new(2, 32 * 1024).expect("could not create Opus encoder");

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

        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(&payload[8..payload_size])?;

        let mkv =
            matroska::Matroska::open(file.reopen().context("failed to reopen temporary file")?)
                .context("failed to open matroska file")?;
        assert_eq!(mkv.info.duration, None);
        assert_eq!(mkv.tracks.len(), 1);
        assert_eq!(mkv.tracks[0].tracktype, matroska::Tracktype::Audio);
        assert_eq!(mkv.tracks[0].codec_id, "A_OPUS");
        if let matroska::Settings::Audio(audio) = &mkv.tracks[0].settings {
            assert_eq!(audio.sample_rate, enc.sample_rate() as f64);
            assert_eq!(audio.channels, enc.channels() as u64);
        } else {
            assert!(
                false,
                "failed to parse audio track: {:?}",
                mkv.tracks[0].settings
            );
        }

        Ok(())
    }
}
