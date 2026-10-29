use std::{
    collections::VecDeque,
    io::Cursor,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use songbird::input::{
    Input, LiveInput, Parsed,
    codecs::{get_codec_registry, get_probe},
};
use symphonia_core::{
    codecs::DecoderOptions,
    errors::{Error as SymphError, SeekErrorKind},
    formats::{Cue, FormatOptions, FormatReader, Packet, SeekMode, SeekTo, SeekedTo, Track},
    io::{MediaSourceStream, MediaSourceStreamOptions},
    meta::{Metadata, MetadataLog, MetadataOptions},
    probe::Hint,
};

const SAMPLE_RATE: u32 = 48_000;
const FRAME_SAMPLES: u64 = 960;
const MAX_QUEUED_PACKETS: usize = 48;
const MAX_PACKET_BYTES: usize = 4_000;
const STARTUP_DELAY: Duration = Duration::from_millis(60);
const MAX_GAP_SAMPLES: u32 = 5_760;
const SILENCE_OPUS: &[u8] = &[0xf8, 0xff, 0xfe];

#[derive(Clone)]
struct QueuedPacket {
    sequence: u16,
    timestamp: u32,
    payload: Vec<u8>,
    samples: u32,
}

#[derive(Default)]
struct QueueState {
    generation: u64,
    packets: VecDeque<QueuedPacket>,
    first_arrival: Option<Instant>,
    expected_sequence: Option<u16>,
    expected_timestamp: Option<u32>,
    next_timestamp: u64,
    startup_complete: bool,
    closed: bool,
    silence_since_packet: u32,
    media_callback: Option<Arc<dyn Fn(u64) + Send + Sync>>,
}

/// A bounded live Opus input pulled synchronously at Songbird's mixer cadence.
/// Twenty-millisecond packets retain their original bytes, enabling Songbird's
/// Opus passthrough path without a PCM reader or Symphonia read-ahead buffer.
#[derive(Clone, Default)]
pub(crate) struct OpusSource {
    state: Arc<Mutex<QueueState>>,
    input: Arc<Mutex<Option<Input>>>,
}

impl OpusSource {
    pub(crate) fn new() -> Result<Self> {
        // Songbird's Parsed input needs probed metadata. Probe a tiny DCA header
        // once to obtain the same public Opus track parameters used by DCA input.
        let dca_meta = br#"{"dca":{"version":1,"tool":{"name":"Codex","version":"1"}},"opus":{"mode":"audio","sample_rate":48000,"frame_size":960,"vbr":true,"channels":2}}"#;
        let mut header = b"DCA1".to_vec();
        header.extend_from_slice(&(dca_meta.len() as u32).to_le_bytes());
        header.extend_from_slice(dca_meta);
        let mss = MediaSourceStream::new(
            Box::new(Cursor::new(header)),
            MediaSourceStreamOptions::default(),
        );
        let probed = get_probe()
            .format(
                &Hint::new(),
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .context("create live Opus track parameters")?;
        let track = probed
            .format
            .default_track()
            .context("Songbird DCA probe returned no Opus track")?;
        let track_id = track.id;
        let params = track.codec_params.clone();
        let decoder = get_codec_registry()
            .make(&params, &DecoderOptions::default())
            .context("create Songbird Opus decoder for live input")?;

        let empty_mss = MediaSourceStream::new(
            Box::new(Cursor::new(Vec::<u8>::new())),
            MediaSourceStreamOptions::default(),
        );
        let state = Arc::new(Mutex::new(QueueState::default()));
        let input = Arc::new(Mutex::new(None));
        let format = PacketReader::new(empty_mss, Arc::clone(&state), track);
        *input.lock().expect("live Opus input slot poisoned") = Some(Input::Live(
            LiveInput::Parsed(Parsed {
                format: Box::new(format),
                decoder,
                track_id,
                meta: probed.metadata,
                supports_backseek: false,
            }),
            None,
        ));
        Ok(Self { state, input })
    }

    /// Take the prepared input once. The caller should pass this to
    /// `Call::play_input`; clones of this source continue sharing the queue.
    pub(crate) fn input(&self) -> Result<Input> {
        self.input
            .lock()
            .expect("live Opus input slot poisoned")
            .take()
            .context("live Opus input was already taken")
    }

    /// Push a received RTP Opus packet into the bounded live queue.
    pub(crate) fn push(
        &self,
        generation: u64,
        sequence: u16,
        timestamp: u32,
        payload: &[u8],
        now: Instant,
    ) -> Result<()> {
        ensure!(!payload.is_empty(), "empty Opus RTP payload");
        ensure!(
            payload.len() <= MAX_PACKET_BYTES,
            "Opus RTP payload is too large"
        );
        let samples = opus2::packet::get_nb_samples(payload, SAMPLE_RATE)
            .context("read native Opus packet duration")? as u32;
        ensure!(
            samples > 0 && samples <= 5_760,
            "unsupported native Opus packet duration"
        );

        let mut state = self.state.lock().expect("live Opus queue poisoned");
        if state.closed {
            return Ok(());
        }
        if state.generation != generation {
            return Ok(());
        }
        if state
            .packets
            .iter()
            .any(|queued| queued.sequence == sequence)
        {
            return Ok(());
        }
        if let Some(expected) = state.expected_sequence {
            if (sequence.wrapping_sub(expected) as i16) < 0 {
                return Ok(());
            }
        }
        if state.packets.len() >= MAX_QUEUED_PACKETS {
            // Retain the newest live audio instead of accumulating latency.
            state.packets.pop_front();
        }
        state.first_arrival.get_or_insert(now);
        let packet = QueuedPacket {
            sequence,
            timestamp,
            payload: payload.to_vec(),
            samples,
        };
        let origin = state.expected_sequence.or_else(|| {
            state
                .packets
                .front()
                .map(|old| old.sequence)
                .or(Some(sequence))
        });
        let offset = sequence.wrapping_sub(origin.unwrap_or(sequence)) as i16;
        let index = state
            .packets
            .iter()
            .position(|old| (old.sequence.wrapping_sub(origin.unwrap_or(sequence)) as i16) > offset)
            .unwrap_or(state.packets.len());
        state.packets.insert(index, packet);
        Ok(())
    }

    pub(crate) fn interrupt(&self, generation: u64) {
        let mut state = self.state.lock().expect("live Opus queue poisoned");
        state.generation = generation;
        state.packets.clear();
        state.first_arrival = None;
        state.expected_sequence = None;
        state.expected_timestamp = None;
        state.startup_complete = false;
        state.closed = false;
        state.silence_since_packet = 0;
    }

    pub(crate) fn close(&self) {
        self.state.lock().expect("live Opus queue poisoned").closed = true;
    }

    pub(crate) fn set_media_callback(&self, callback: Arc<dyn Fn(u64) + Send + Sync>) {
        self.state
            .lock()
            .expect("live Opus queue poisoned")
            .media_callback = Some(callback);
    }
}

struct PacketReader {
    source: MediaSourceStream,
    state: Arc<Mutex<QueueState>>,
    track: Track,
    cues: Vec<Cue>,
    metadata: MetadataLog,
}

impl PacketReader {
    fn new(source: MediaSourceStream, state: Arc<Mutex<QueueState>>, track: &Track) -> Self {
        Self {
            source,
            state,
            track: track.clone(),
            cues: Vec::new(),
            metadata: MetadataLog::default(),
        }
    }

    fn take_packet(&self, now: Instant) -> Option<Packet> {
        let mut state = self.state.lock().expect("live Opus queue poisoned");
        if state.closed && state.packets.is_empty() {
            return None;
        }
        let startup_ready = state
            .first_arrival
            .is_some_and(|arrival| now.saturating_duration_since(arrival) >= STARTUP_DELAY);
        if !state.startup_complete && !startup_ready {
            return Some(silence_packet(&mut state, self.track.id));
        }
        state.startup_complete = true;

        let packet = state.packets.pop_front();
        let Some(packet) = packet else {
            return (!state.closed).then(|| silence_packet(&mut state, self.track.id));
        };

        if let Some(expected_sequence) = state.expected_sequence {
            let sequence_gap = packet.sequence.wrapping_sub(expected_sequence) as i16;
            let timestamp_gap = state
                .expected_timestamp
                .map(|expected| packet.timestamp.wrapping_sub(expected) as i32);
            let remaining_gap = timestamp_gap
                .map(|gap| (gap.max(0) as u32).saturating_sub(state.silence_since_packet));
            if sequence_gap > 0
                && remaining_gap.is_some_and(|gap| {
                    gap >= FRAME_SAMPLES as u32
                        && gap <= MAX_GAP_SAMPLES
                        && gap.is_multiple_of(FRAME_SAMPLES as u32)
                })
            {
                // A missing 20 ms packet becomes one frame of valid Opus
                // silence. Subsequent mixer pulls consume the queued packet.
                state.packets.push_front(packet);
                state.expected_sequence = Some(expected_sequence.wrapping_add(1));
                let ts = state.next_timestamp;
                state.next_timestamp = state.next_timestamp.wrapping_add(FRAME_SAMPLES);
                state.silence_since_packet = state.silence_since_packet.saturating_add(960);
                return Some(Packet::new_from_slice(
                    self.track.id,
                    ts,
                    FRAME_SAMPLES,
                    SILENCE_OPUS,
                ));
            }
        }

        state.expected_sequence = Some(packet.sequence.wrapping_add(1));
        state.expected_timestamp = Some(packet.timestamp.wrapping_add(packet.samples));
        state.silence_since_packet = 0;
        state.next_timestamp = state.next_timestamp.wrapping_add(u64::from(packet.samples));
        let output = Packet::new_from_slice(
            self.track.id,
            state
                .next_timestamp
                .saturating_sub(u64::from(packet.samples)),
            u64::from(packet.samples),
            &packet.payload,
        );
        let callback = if packet.payload.as_slice() == SILENCE_OPUS {
            None
        } else {
            state.media_callback.clone()
        };
        let samples = u64::from(packet.samples);
        drop(state);
        if let Some(callback) = callback {
            callback(samples);
        }
        Some(output)
    }
}

fn silence_packet(state: &mut QueueState, track_id: u32) -> Packet {
    let ts = state.next_timestamp;
    state.next_timestamp = state.next_timestamp.wrapping_add(FRAME_SAMPLES);
    if state.expected_timestamp.is_some() {
        state.silence_since_packet = state.silence_since_packet.saturating_add(960);
    }
    Packet::new_from_slice(track_id, ts, FRAME_SAMPLES, SILENCE_OPUS)
}

impl FormatReader for PacketReader {
    fn try_new(_: MediaSourceStream, _: &FormatOptions) -> symphonia_core::errors::Result<Self> {
        Err(SymphError::Unsupported(
            "live Opus source is constructed directly",
        ))
    }

    fn cues(&self) -> &[Cue] {
        &self.cues
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn seek(&mut self, _: SeekMode, _: SeekTo) -> symphonia_core::errors::Result<SeekedTo> {
        Err(SymphError::SeekError(SeekErrorKind::Unseekable))
    }

    fn tracks(&self) -> &[Track] {
        std::slice::from_ref(&self.track)
    }

    fn next_packet(&mut self) -> symphonia_core::errors::Result<Packet> {
        self.take_packet(Instant::now()).ok_or_else(|| {
            SymphError::IoError(std::io::Error::from(std::io::ErrorKind::UnexpectedEof))
        })
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.source
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opus2::{Application, Channels, Encoder};

    fn encoded(samples: usize, seed: i16) -> Vec<u8> {
        let mut encoder = Encoder::new(SAMPLE_RATE, Channels::Stereo, Application::Voip).unwrap();
        let pcm: Vec<i16> = (0..samples * 2)
            .map(|i| seed.wrapping_add(i as i16 * 3))
            .collect();
        let mut bytes = vec![0_u8; MAX_PACKET_BYTES];
        let len = encoder.encode(&pcm, &mut bytes).unwrap();
        bytes.truncate(len);
        bytes
    }

    fn take_parsed(input: Input) -> Parsed {
        let Input::Live(LiveInput::Parsed(parsed), None) = input else {
            panic!("expected direct live Opus input");
        };
        parsed
    }

    #[test]
    fn twenty_millisecond_frame_reaches_songbird_unchanged() {
        let source = OpusSource::new().unwrap();
        let payload = encoded(960, 71);
        source
            .push(0, 7, 4000, &payload, Instant::now() - STARTUP_DELAY)
            .unwrap();
        let mut parsed = take_parsed(source.input().unwrap());

        let packet = parsed.format.next_packet().unwrap();
        assert_eq!(packet.buf(), payload);
        assert_eq!(packet.dur, FRAME_SAMPLES);
    }

    #[test]
    fn ten_millisecond_frame_keeps_native_duration_for_songbird_fallback() {
        let source = OpusSource::new().unwrap();
        let payload = encoded(480, 19);
        source
            .push(0, 7, 4000, &payload, Instant::now() - STARTUP_DELAY)
            .unwrap();
        let mut parsed = take_parsed(source.input().unwrap());

        let packet = parsed.format.next_packet().unwrap();
        assert_eq!(packet.buf(), payload);
        assert_eq!(packet.dur, 480);
    }

    #[test]
    fn interruption_discards_old_packets_and_accepts_new_generation() {
        let source = OpusSource::new().unwrap();
        let old = encoded(960, 3);
        let fresh = encoded(960, 29);
        let arrived = Instant::now() - STARTUP_DELAY;
        source.push(0, 10, 1000, &old, arrived).unwrap();
        source.interrupt(1);
        source.push(1, 50, 9000, &fresh, arrived).unwrap();
        source.push(0, 11, 1960, &old, arrived).unwrap();
        let mut parsed = take_parsed(source.input().unwrap());

        let packet = parsed.format.next_packet().unwrap();
        assert_eq!(packet.buf(), fresh);
    }

    #[test]
    fn queue_stays_bounded_and_retains_recent_packets() {
        let source = OpusSource::new().unwrap();
        let payload = encoded(960, 7);
        let arrived = Instant::now() - STARTUP_DELAY;
        for sequence in 0..(MAX_QUEUED_PACKETS as u16 + 8) {
            source
                .push(
                    0,
                    sequence,
                    u32::from(sequence) * FRAME_SAMPLES as u32,
                    &payload,
                    arrived,
                )
                .unwrap();
        }
        let state = source.state.lock().unwrap();
        assert_eq!(state.packets.len(), MAX_QUEUED_PACKETS);
        assert_eq!(
            state.packets.back().unwrap().sequence,
            MAX_QUEUED_PACKETS as u16 + 7
        );
    }

    #[test]
    fn startup_reorders_earlier_sequence_and_handles_wraparound() {
        let arrived = Instant::now() - STARTUP_DELAY;
        let source = OpusSource::new().unwrap();
        let second = encoded(960, 20);
        let first = encoded(960, 10);
        source.push(0, 2, 1960, &second, arrived).unwrap();
        source.push(0, 1, 1000, &first, arrived).unwrap();
        let mut parsed = take_parsed(source.input().unwrap());
        assert_eq!(parsed.format.next_packet().unwrap().buf(), first);
        assert_eq!(parsed.format.next_packet().unwrap().buf(), second);

        let wrapped = OpusSource::new().unwrap();
        let seq_zero = encoded(960, 40);
        let seq_max = encoded(960, 30);
        wrapped.push(0, 0, 1960, &seq_zero, arrived).unwrap();
        wrapped.push(0, u16::MAX, 1000, &seq_max, arrived).unwrap();
        let mut parsed = take_parsed(wrapped.input().unwrap());
        assert_eq!(parsed.format.next_packet().unwrap().buf(), seq_max);
        assert_eq!(parsed.format.next_packet().unwrap().buf(), seq_zero);
    }

    #[test]
    fn close_before_first_packet_ends_instead_of_emitting_silence_forever() {
        let source = OpusSource::new().unwrap();
        let mut parsed = take_parsed(source.input().unwrap());
        source.close();
        assert!(parsed.format.next_packet().is_err());
    }

    #[test]
    fn media_callback_counts_frames_when_songbird_pulls_them() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let source = OpusSource::new().unwrap();
        let delivered = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&delivered);
        source.set_media_callback(Arc::new(move |samples| {
            counter.fetch_add(samples, Ordering::Relaxed);
        }));
        let payload = encoded(960, 14);
        source
            .push(0, 1, 960, &payload, Instant::now() - STARTUP_DELAY)
            .unwrap();
        assert_eq!(delivered.load(Ordering::Relaxed), 0);

        let mut parsed = take_parsed(source.input().unwrap());
        assert_eq!(parsed.format.next_packet().unwrap().buf(), payload);
        assert_eq!(delivered.load(Ordering::Relaxed), FRAME_SAMPLES);
    }

    #[test]
    fn missing_packet_fill_subtracts_silence_already_emitted_on_underrun() {
        let source = OpusSource::new().unwrap();
        let first = encoded(960, 1);
        let next = encoded(960, 2);
        let arrived = Instant::now() - STARTUP_DELAY;
        source.push(0, 10, 1000, &first, arrived).unwrap();
        let mut parsed = take_parsed(source.input().unwrap());

        assert_eq!(parsed.format.next_packet().unwrap().buf(), first);
        assert_eq!(parsed.format.next_packet().unwrap().buf(), SILENCE_OPUS);
        assert_eq!(parsed.format.next_packet().unwrap().buf(), SILENCE_OPUS);
        source.push(0, 12, 2920, &next, Instant::now()).unwrap();
        // The one-frame RTP gap has already been covered by underflow silence.
        assert_eq!(parsed.format.next_packet().unwrap().buf(), next);
    }
}
