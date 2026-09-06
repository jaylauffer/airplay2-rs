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
