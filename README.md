# RoomSync

**Early, private Windows → Android LAN-audio prototype.** Native code for
capturing ordinary Windows system playback and sending it to multiple receivers
with a common presentation timeline. Not a finished product, not hardware-certified,
and never a zero-latency promise. Video/call-specific behavior is deferred.

## Try the prototype

Update both the Windows host and Android receiver to **0.1.1**.

- Extract the Windows ZIP and open `Start-RoomSync.cmd`. It opens local controls.
  Windows automatically lists its LAN addresses; no phone IP needs typing.
- Create a private setup code, then scan its locally generated QR in Android or
  paste the whole code. Host IP, ports and key fill automatically.
- Start listening and connect Android in either order. Pairing waits until Stop,
  with packet/authentication diagnostics rather than a ten-second click deadline.
  Windows learns the phone's source address only after authenticated pairing.
- All configured receivers confirm one shared timeline before a five-second warmup.
  Windows' original speaker remains undelayed: keep it inaudible to avoid echo.
- Use a trusted LAN without client isolation. No firewall/interface settings are
  changed automatically. Never publish the setup code or expose UDP ports online.

The Windows executable is unsigned and Android is a test build. Verify source and
checksums; do not bypass security warnings for an untrusted download. Real-device
capture, playback and acoustic synchronization still require validation.

Instructions: [Windows](apps/desktop/README.md), [Android](apps/android/README.md),
[architecture](docs/ARCHITECTURE.md), [wire record](docs/PROTOCOL.md),
[transport](adapters/transport/README.md).

## Implemented code

- A dependency-free, unsafe-forbidden Rust synchronization core with canonical
  bounded packet parsing, fixed reorder storage, clock offset/drift fitting,
  independent DAC-rate estimation, timestamped rendering and bounded concealment.
- 48 kHz stereo PCM16, 120 frames per packet. Audio record 512 bytes, authenticated
  UDP datagram 542 bytes. PCM payload alone 1.536 Mbit/s per receiver.
- Pairwise Noise NNpsk0 handshakes, ChaCha20-Poly1305 authenticated encryption,
  authenticated peer admission, replay windows, explicit common stream ID/epoch and bounded queues.
- WASAPI shared-loopback capture and native playback, requested 48 kHz conversion,
  QPC timestamps and hardware-correlated output-time projection. Clock drift and
  capture idle/discontinuity handling preserve a common source timeline.
- A loopback-only local setup page with offline QR, automatic LAN address selection,
  Start/Stop and observed connection status, plus explicit developer CLI modes. Up to eight
  configured receivers use separate authenticated sockets but one timeline.
- Android receiver source/build is under `apps/android`; it uses the same Rust
  protocol/timing code, not a browser audio approximation.

No paid services, remote audio cloud relay, microphone capture, virtual audio
driver installation or automatic OS audio-setting changes are part of this code.
Protected streams can be unavailable. True multichannel 5.1/7.1 is not synthesized
from stereo. Bluetooth/DSP latency is not automatically acoustically calibrated.

## What the cloud tests establish

Core tests cover signed PCM/format validation, truncation and random malformed
inputs, extreme timestamps, replay-window-like buffer admission, reorder/loss,
RTT transitions, stale mappings, independent DAC drift, channel mapping and
virtual timeline bounds. Desktop tests cover capture packetization, drift,
per-block timestamp jitter, idle/resume, explicit gaps and command validation.
Transport tests exercise real localhost UDP including wrong keys/tampering/replay.

Windows cross-compilation/linking and Android APK builds establish buildability,
**not** successful physical capture, actual speaker skew, latency, battery use,
Bluetooth behavior, call audio, or app-store readiness. Numerical simulation
results are labeled virtual and must never be reported as measured speaker sync.
The interpolation/resampler and cryptographic integration need further independent
review and hardware measurements before general release.

## Developer checks

Rust is pinned in `rust-toolchain.toml`. Each adapter/app has a standalone manifest.

```
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo run --release --locked -- simulate 30
cargo test --manifest-path adapters/transport/Cargo.toml --all-targets --locked
cargo test --manifest-path adapters/windows/Cargo.toml --all-targets --locked
cargo test --manifest-path apps/desktop/Cargo.toml --all-targets --locked
```

Use Windows to execute its device adapter. A Linux build returns explicit
unsupported-platform errors for real capture/playback. See each adapter/app README
for additional native build tools. CI definitions are provided; a definition in
the repository is not evidence that remote CI has run or passed.

This private project has no open-source license selected. Third-party dependency
licenses remain their respective owners' terms. Keep key/signing files and build
outputs out of version control.
