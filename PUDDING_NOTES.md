# Notes for this vendored copy

This is a local clone of [lmcgartland/airplay2-rs](https://github.com/lmcgartland/airplay2-rs)
(GPL-2.0), used by [`sng-bass-blaster`](../sng-bass-blaster)'s "Broadcast"
(AirPlay) output feature.

## Why this exists

macOS has no supported, working third-party API to enumerate or select a
specific AirPlay speaker (confirmed 2026-09-06: `AVRoutePickerView` depends
on a private `AirPlayXPCHelper` XPC service third-party apps can't use, and
CoreAudio's `kAudioHardwarePropertyDevices` hasn't listed AirPlay devices
since macOS 10.11). This project implements the real AirPlay 2 protocol
(HomeKit transient pairing, RTSP/RTP, ChaCha20-Poly1305 audio encryption)
directly, using documented protocol capabilities rather than any exploit.
See `sng-bass-blaster/docs/UI_INPUT_FINDINGS.md` for the full
investigation that led here.

## License boundary -- read before touching this

**GPL-2.0.** `sng-bass-blaster` treats this strictly as an external
process, spawned via `std::process::Command` and talked to over stdin/
stdout -- never linked as a Cargo dependency. That keeps GPL's copyleft
contained to this directory rather than propagating into
`sng-bass-blaster` or anything in `loadngo`. **Do not add this crate (or
any crate from it) as a `path`/git Cargo dependency of `sng-bass-blaster`
or `loadngo` -- that would change the license obligations of the whole
binary it's linked into.**

This is also why `sng-bass-blaster` is treated as personal-use-only, not
distributed -- see that repo's README.

## What was added on top of upstream

Two example binaries, written for `sng-bass-blaster`'s use, not part of
upstream:

- `crates/airplay-discovery/examples/list_json.rs` -- scans and prints one
  JSON object per line (name/id/ip/port/airplay2/model). Kept separate
  from `debug_devices.rs` (human-readable) so `sng-bass-blaster` has a
  stable, parseable format to consume as a subprocess.
- `crates/airplay-client/examples/live_stdin_sender.rs` -- reads raw
  interleaved i16 LE PCM continuously from stdin and streams it to a
  given `<ip> <port>` using this project's own `LiveAudioDecoder` +
  `Connection::start_streaming_live` (the same live-source path upstream
  built for its Bluetooth-capture-to-AirPlay feature). Prints `READY` on
  stdout once streaming has started. Requires stereo (2-channel) input at
  the sample rate passed via `--sample-rate`; mono sources must be
  duplicated to stereo before piping in (this binary does not upmix).

Both were verified end-to-end against a real HomePod mini
("AudioAccessory5,1") on 2026-09-06: `list_json` correctly named and
addressed it, and `live_stdin_sender` produced real, audibly-confirmed
sound through it.

## Building the binaries `sng-bass-blaster` spawns

```bash
cargo build --release -p airplay-discovery --example list_json
cargo build --release -p airplay-client --example live_stdin_sender
```

Produces `target/release/examples/list_json` and
`target/release/examples/live_stdin_sender`. `sng-bass-blaster` expects
these paths relative to this directory -- rebuild here after pulling
upstream changes or editing either example.

## Known quirks observed

- A stale/uncleanly-terminated previous session to the same device can
  cause the next `SETUP`/`RECORD` to time out for several seconds before
  the device accepts a new connection. Not something this vendored copy
  works around; just retry.
- mDNS parsing occasionally warns `invalid UUID: <a>+<b>` for a
  multi-room-group service announcement variant; harmless -- the device's
  primary AirPlay service record still parses and resolves correctly.
- **Real bug, found and fixed 2026-09-06:** a receiver can silently mute
  itself mid-stream if its incoming audio stalls for a few seconds (e.g. a
  buffer underrun from the sender side going quiet). The actual root cause
  that day was entirely on the `sng-bass-blaster` side (a blocking
  discovery scan on its main render loop -- see that repo's
  `docs/UI_INPUT_FINDINGS.md`), but `live_stdin_sender.rs`'s reader thread
  was hardened at the same time: it now uses `LiveFrameSender::try_send`
  (drop-on-backpressure) instead of the blocking `send`, so a slow/stuck
  `AudioStreamer` consumer can no longer stall this thread's `read_exact`
  loop and back-pressure the parent process's stdin writes. If a receiver
  ever mutes again with the fixed parent-side scheduling, check for
  dropped-frame warnings from this reader thread first before assuming a
  new root cause.

## Real bug, found and fixed 2026-09-09: sync packets declared zero latency

`sng-bass-blaster` reported broadcasting to a HomePod working for about
five seconds, going choppy, then shutting off. The cause was here, not
there, and it affected every session to that device.

`RtpSender::send_sync`/`prepare_sync` in
`crates/airplay-audio/src/rtp.rs` built the 20-byte RAOP sync packet with
both RTP timestamp fields set to the same value:

```rust
packet[4..8]   = rtp_timestamp;   // "RTP timestamp less latency"
packet[16..20] = rtp_timestamp;   // "RTP timestamp now"
```

Bytes 4-7 tell the receiver which timestamp it should be **rendering** at
the NTP instant carried in bytes 8-15. Filling them with the current
timestamp declares a render latency of zero -- the receiver is told to
play every packet the instant it arrives. `RtpSender` carried no latency
value at all, so there was nothing to subtract. A HomePod renders its
initial prefill, then chops, then mutes the session.

**Nothing on the sender side reports this.** RTSP `feedback` keeps
returning 200 OK, the NTP timing server keeps answering the receiver's
requests every ~2.5s, sync packets keep going out on schedule (verified:
51 of them across a 50s run, one per ~44352 frames), and RTP keeps
flowing indefinitely. The receiver also never sends anything back -- zero
retransmit requests -- so there is no inbound signal to notice either.

**Fix:**

- `RtpSender` gains a `sync_latency: u32` field plus `set_sync_latency`,
  defaulting to 0 so existing callers and unit tests are unaffected.
- Both NTP sync builders now emit `rtp_timestamp - sync_latency` in
  bytes 4-7. `prepare_ptp_sync`/`send_ptp_sync` are untouched (different
  packet layout, and PTP mode is separately broken -- see below).
- `streamer.rs` sets it from `config.latency_min` at each sync.
- `crates/airplay-client/examples/live_stdin_sender.rs` now declares
  `latency_min: 11025` (~250ms) instead of `4410` (~100ms). 11025 is
  simply `StreamConfig::default()`'s own value; the example had been
  overriding it downward. Measured against a real HomePod: 0 gave 3.4s of
  audio before permanent silence, 4410 gave 43.7s with a gap, 11025 gave
  a clean 49.2s -- the whole take.

**Verification method** (worth reusing): pipe a real-time-paced synthetic
tone into `live_stdin_sender`, record the room off a webcam mic with
`ffmpeg -f avfoundation`, and score 50ms blocks with a Goertzel filter at
the tone frequency divided by block RMS. That level-independent
"tonality" ratio survives the mic level swinging around; an absolute
amplitude threshold does not. Full write-up, including everything that
was ruled out, in `sng-bass-blaster/docs/UI_INPUT_FINDINGS.md` section 8.

**Known-failing test, pre-existing:** `-p airplay-audio`'s
`test_continuous_streaming_simulation` ("Captured too few periods")
fails identically on an untouched tree; it is timing-sensitive and
unrelated to this change. Confirmed by stashing everything and re-running.

**`--ptp` is non-functional.** It performs a full BMCA negotiation and
correctly yields master to the HomePod (priority1 248 vs our 250), then
logs `gPTP BMCA initialized (offset: 0 ns)` -- no offset against the
master clock is ever computed, and no audio plays at all. NTP timing plus
the sync-latency fix above is the working path.
