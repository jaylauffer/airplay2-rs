//! Stream live PCM audio from stdin to an AirPlay 2 device.
//!
//! Written for `sng-bass-blaster`'s live monitor: the parent process pipes
//! raw interleaved i16 LE PCM into this process's stdin continuously, and
//! this process forwards it over AirPlay 2 using `LiveAudioDecoder` +
//! `Connection::start_streaming_live` -- the same live-source path this
//! project built for its own Bluetooth-capture-to-AirPlay feature.
//!
//! Run with:
//!   cargo run -p airplay-client --example live_stdin_sender -- <ip> <port> [--sample-rate N] [--channels N] [--ptp]
//!
//! Prints "READY" to stdout on its own line once streaming has started, so
//! a parent process knows when it's safe to start writing PCM.

use airplay_audio::{AlacEncoder, LiveAudioDecoder, LivePcmFrame};
use airplay_client::Connection;
use airplay_core::stream::{PtpMode, StreamType, TimingProtocol};
use airplay_core::{AudioCodec, AudioFormat, StreamConfig};
use airplay_discovery::{Discovery, ServiceBrowser};
use std::io::Read;
use std::net::IpAddr;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "Usage: {} <ip> <port> [--sample-rate N] [--channels N] [--ptp]",
            args[0]
        );
        std::process::exit(1);
    }

    let target_ip: IpAddr = args[1].parse()?;
    let target_port: u16 = args[2].parse()?;
    let sample_rate: u32 = args
        .iter()
        .position(|a| a == "--sample-rate")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(44100);
    let channels: u8 = args
        .iter()
        .position(|a| a == "--channels")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let use_ptp = args.iter().any(|a| a == "--ptp");

    eprintln!("Scanning for {}:{}...", target_ip, target_port);
    let browser = ServiceBrowser::new()?;
    let devices = browser.scan(Duration::from_secs(4)).await?;
    let device = devices
        .into_iter()
        .find(|d| d.addresses.contains(&target_ip) && d.port == target_port)
        .ok_or_else(|| format!("device {}:{} not found in mDNS scan", target_ip, target_port))?;
    eprintln!("Found: {} ({})", device.name, device.model);

    let audio_format = AudioFormat::default(); // ALAC 44100Hz/16-bit/stereo
    let asc = if audio_format.codec == AudioCodec::Alac {
        Some(AlacEncoder::new(audio_format.clone())?.magic_cookie())
    } else {
        None
    };

    let config = StreamConfig {
        stream_type: StreamType::Realtime,
        audio_format,
        timing_protocol: if use_ptp {
            TimingProtocol::Ptp
        } else {
            TimingProtocol::Ntp
        },
        ptp_mode: PtpMode::Master,
        // Lower than the file-playback example's 500ms/2s: this is a live
        // monitor, so minimizing added latency matters more than absorbing
        // network jitter for a one-shot file play.
        latency_min: 4410, // ~100ms @ 44100Hz
        latency_max: 22050, // ~500ms @ 44100Hz
        supports_dynamic_stream_id: true,
        asc,
    };

    eprintln!("Connecting (AirPlay 2, auto pairing)...");
    let mut conn = Connection::connect_auto(device, config, "3939").await?;
    eprintln!("Connected. Setting up stream...");
    conn.setup().await?;

    // 64 frames of ~1024-sample chunks is a few hundred ms of headroom
    // against the parent process's feed being bursty (it polls its own
    // capture tap once per render frame, not in perfectly steady
    // real-time slices) rather than a hard real-time source.
    let (sender, live_decoder) = LiveAudioDecoder::create_pair(sample_rate, channels, 64);

    // Reader thread: raw interleaved i16 LE PCM from stdin -> LivePcmFrame.
    // A dedicated std::thread (not a tokio task) since std::io::Stdin's
    // blocking read is the simplest way to backpressure against a parent
    // process writing at the real capture rate.
    //
    // Uses `try_send` (drop-oldest-effectively, since a full channel just
    // means this frame is skipped), not the blocking `send`. A blocking
    // send here would, under sustained backpressure from a slow/stalled
    // AudioStreamer consumer, stall this thread's `read_exact` loop --
    // which stalls the parent process's writes to our stdin pipe once its
    // OS pipe buffer fills, which stalls whatever the parent uses to
    // decide it's safe to keep capturing (see
    // sng-bass-blaster/docs/UI_INPUT_FINDINGS.md for the specific stall
    // that motivated this: a periodic ~4s blocking scan on the parent's
    // main loop, fixed there, but this side of the pipe should not be
    // able to propagate a stall either). A dropped live-audio frame is a
    // brief glitch; a stalled pipe is a stuck stream that needs a manual
    // reconnect.
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut lock = stdin.lock();
        let frames_per_chunk = 1024;
        let mut byte_buf = vec![0u8; frames_per_chunk * channels as usize * 2];
        let mut dropped_frames = 0u64;
        loop {
            match lock.read_exact(&mut byte_buf) {
                Ok(()) => {
                    let samples: Vec<i16> = byte_buf
                        .chunks_exact(2)
                        .map(|b| i16::from_le_bytes([b[0], b[1]]))
                        .collect();
                    let frame = LivePcmFrame {
                        samples,
                        channels,
                        sample_rate,
                    };
                    if !sender.try_send(frame) {
                        dropped_frames += 1;
                        if dropped_frames.is_multiple_of(100) {
                            eprintln!(
                                "live decoder backpressured, dropped {dropped_frames} frames so far"
                            );
                        }
                    }
                }
                Err(error) => {
                    eprintln!("stdin closed ({error}), stopping reader thread");
                    break;
                }
            }
        }
    });

    conn.start_streaming_live(live_decoder).await?;
    println!("READY");
    eprintln!("Streaming...");

    let mut feedback_tick = 0u32;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        feedback_tick += 1;
        if feedback_tick % 2 == 0 {
            if let Err(error) = conn.send_feedback().await {
                tracing::warn!("Feedback failed: {}", error);
            }
        }
    }
}
