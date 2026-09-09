//! Round-trip validation of `AlacEncoder`'s output against a real ALAC
//! decoder (symphonia's), over a long run.
//!
//! ALAC is lossless, so decoded samples must be bit-exact with the
//! source PCM. Nothing in this crate's existing tests ever decodes the
//! encoder's own output -- they only inspect encoded byte counts -- so a
//! bitstream-corruption bug in the encoder itself would have been
//! invisible to the test suite. This test exists to find exactly that,
//! motivated by a real-world report of audible "static" appearing after
//! roughly 5-15 seconds of continuous AirPlay streaming while every
//! other layer (RTP delivery, encryption, feedback, timing) stayed
//! healthy.

use airplay_audio::{AlacEncoder, AudioEncoder};
use airplay_core::{AudioFormat, AudioCodec, SampleRate};
use symphonia::core::codecs::{CodecParameters, DecoderOptions, CODEC_TYPE_ALAC};
use symphonia::core::formats::Packet;
use symphonia::core::audio::{AudioBufferRef, Signal};

fn format_44100_stereo() -> AudioFormat {
    AudioFormat {
        codec: AudioCodec::Alac,
        sample_rate: SampleRate::Hz44100,
        bit_depth: 16,
        channels: 2,
        frames_per_packet: 352,
    }
}

/// Deterministic pseudo-random generator (no external crate needed) so
/// the signal isn't a pure tone -- a real bass signal has broadband,
/// changing content, and a pure sine could fail to exercise whatever
/// code path is actually corrupting state.
struct Lcg(u64);
impl Lcg {
    fn next_i16(&mut self) -> i16 {
        // Numerical Recipes LCG constants.
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 48) as i16) / 4 // keep amplitude away from full-scale
    }
}

/// Mixed sine + noise signal, closer to a real bass/amp signal than a
/// pure tone, generated for `duration_secs` seconds of interleaved
/// stereo i16 PCM at 44100Hz.
fn generate_signal(duration_secs: u32, sample_rate: u32) -> Vec<i16> {
    let total_frames = (duration_secs * sample_rate) as usize;
    let mut out = Vec::with_capacity(total_frames * 2);
    let mut rng = Lcg(0xDEADBEEFCAFEu64);
    for n in 0..total_frames {
        let t = n as f64 / sample_rate as f64;
        // A bass-ish fundamental with harmonics, plus a little noise --
        // deliberately not a clean single sine wave.
        let tone = (2.0 * std::f64::consts::PI * 110.0 * t).sin() * 9000.0
            + (2.0 * std::f64::consts::PI * 220.0 * t).sin() * 3000.0
            + (2.0 * std::f64::consts::PI * 55.0 * t).sin() * 1500.0;
        let noise = rng.next_i16() as f64;
        let sample = (tone + noise).clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        out.push(sample); // left
        out.push(sample); // right (identical channels keeps comparison simple)
    }
    out
}

fn audio_buffer_to_i16(buffer: &AudioBufferRef) -> Vec<i16> {
    match buffer {
        AudioBufferRef::S16(buf) => {
            let mut samples = Vec::with_capacity(buf.frames() * buf.spec().channels.count());
            for frame in 0..buf.frames() {
                for ch in 0..buf.spec().channels.count() {
                    samples.push(buf.chan(ch)[frame]);
                }
            }
            samples
        }
        AudioBufferRef::S32(buf) => {
            let mut samples = Vec::with_capacity(buf.frames() * buf.spec().channels.count());
            for frame in 0..buf.frames() {
                for ch in 0..buf.spec().channels.count() {
                    samples.push((buf.chan(ch)[frame] >> 16) as i16);
                }
            }
            samples
        }
        other => panic!("unexpected decoded sample format: {:?}", other.spec()),
    }
}

#[test]
fn alac_round_trip_stays_bit_exact_over_a_long_run() {
    let format = format_44100_stereo();
    let sample_rate = format.sample_rate.as_hz();
    let mut encoder = AlacEncoder::new(format.clone()).expect("create encoder");
    let magic_cookie = encoder.magic_cookie();

    let mut params = CodecParameters::new();
    params
        .for_codec(CODEC_TYPE_ALAC)
        .with_sample_rate(sample_rate)
        .with_extra_data(magic_cookie.into_boxed_slice());
    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .expect("create ALAC decoder");

    // 30 seconds -- comfortably past the 5-15s window the real bug was
    // reported in.
    let duration_secs = 30;
    let source = generate_signal(duration_secs, sample_rate);

    let samples_per_packet = format.frames_per_packet as usize * format.channels as usize;
    let mut frame_ts: u64 = 0;
    let mut first_divergence: Option<(usize, usize)> = None; // (packet_index, sample_index)
    let mut packets_checked = 0usize;

    for (packet_index, chunk) in source.chunks(samples_per_packet).enumerate() {
        if chunk.len() != samples_per_packet {
            break; // trailing partial chunk, not a full packet
        }
        let encoded = encoder.encode(chunk).expect("encode packet");

        let packet = Packet::new_from_slice(0, frame_ts, format.frames_per_packet as u64, &encoded.data);
        frame_ts += format.frames_per_packet as u64;

        let decoded_buf = decoder.decode(&packet).unwrap_or_else(|e| {
            panic!("decode failed at packet {packet_index} (ts={frame_ts}): {e}")
        });
        let decoded = audio_buffer_to_i16(&decoded_buf);

        packets_checked += 1;

        if decoded.len() != chunk.len() {
            first_divergence = Some((packet_index, 0));
            eprintln!(
                "packet {packet_index}: length mismatch, decoded {} vs source {}",
                decoded.len(),
                chunk.len()
            );
            break;
        }

        if let Some(sample_index) = (0..chunk.len()).find(|&i| decoded[i] != chunk[i]) {
            first_divergence = Some((packet_index, sample_index));
            eprintln!(
                "packet {packet_index}, sample {sample_index}: decoded={} source={} (encoded_len={}, first_8={:?})",
                decoded[sample_index],
                chunk[sample_index],
                encoded.data.len(),
                &encoded.data[..8.min(encoded.data.len())]
            );
            break;
        }
    }

    println!(
        "Checked {packets_checked} packets ({:.1}s) before {}",
        packets_checked as f64 * format.frames_per_packet as f64 / sample_rate as f64,
        if first_divergence.is_some() { "divergence" } else { "end of signal (no divergence)" }
    );

    assert!(
        first_divergence.is_none(),
        "ALAC round-trip diverged from source at {:?} -- encoder corrupted its own bitstream",
        first_divergence
    );
}
