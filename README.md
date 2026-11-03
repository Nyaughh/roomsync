# RoomSync

**Early, private Windows → Android LAN-audio prototype.** Native code for
capturing ordinary Windows system playback and sending it to multiple receivers
with a common presentation timeline. Not a finished product, not hardware-certified,
and never a zero-latency promise. Video/call-specific behavior is deferred.

## Try the prototype

- Windows: extract the supplied Windows ZIP and open `Start-RoomSync.cmd`. The
  guided setup asks for phone IP addresses, selects the local LAN route, creates a
  pairing key and reveals it only when you explicitly ask. No system settings or
  firewall rules are automatically changed.
- Android: install the separately supplied test APK, then enter the host/receiver
  ports, private pairing key and stereo/left/right/mono channel. See the Android
  app's README for its current lifecycle and installation limits.
- Both devices must be on a trusted LAN that allows device-to-device traffic.
  Never expose the UDP ports to the public internet or post pairing keys.
- Windows' original speaker output is **not delayed or muted**. Keep that output
  inaudible (for example, headphones) to avoid an echo with the delayed receivers.
- The Windows binary is unsigned and the Android package is a test build. Do not
  bypass security warnings for a download you do not trust. Verify source and
  supplied SHA-256 checksums. Physical device testing is still required.

Detailed instructions: [Windows](apps/desktop/README.md),
[Android](apps/android/README.md), [architecture](docs/ARCHITECTURE.md),
[wire record](docs/PROTOCOL.md), [transport](adapters/transport/README.md).

## Implemented code

- A dependency-free, unsafe-forbidden Rust synchronization core with canonical
  bounded packet parsing, fixed reorder storage, clock offset/drift fitting,
  independent DAC-rate estimation, timestamped rendering and bounded concealment.
- 48 kHz stereo PCM16, 120 frames per packet. Audio record 512 bytes, authenticated
  UDP datagram 542 bytes. PCM payload alone 1.536 Mbit/s per receiver.
- Pairwise Noise NNpsk0 handshakes, ChaCha20-Poly1305 authenticated encryption,
  fixed peers, replay windows, explicit common stream ID/epoch and bounded queues.
- WASAPI shared-loopback capture and native playback, requested 48 kHz conversion,
  QPC timestamps and hardware-correlated output-time projection. Clock drift and
  capture idle/discontinuity handling preserve a common source timeline.
- Desktop guided host setup plus explicit CLI host/receiver modes. Up to eight
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
