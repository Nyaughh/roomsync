# RoomSync

**Early, private Windows → Android LAN-audio prototype.** Native code for
capturing ordinary Windows system playback and sending it to multiple receivers
with a common presentation timeline. Not a finished product, not hardware-certified,
and never a zero-latency promise. Video/call-specific behavior is deferred.

## Try the prototype

Use Windows **0.1.5** with Android **0.1.4**. Windows 0.1.5 adds opt-in local
speaker playback through a manually configured virtual source. Android does not
need replacing for this feature. Protocol revision 3 remains compatible with
0.1.2–0.1.4, but older hosts retain their previous features and delay settings.
Versions older than 0.1.2 do not support output-budget negotiation.

- Extract the Windows ZIP and open `Start-RoomSync.cmd`. It opens local controls.
  Windows automatically lists its LAN addresses; no phone IP needs typing.
- Create a private setup code, then scan its locally generated QR in Android or
  paste the whole code. Host IP, ports and key fill automatically.
- Start listening and connect Android in either order. Pairing waits until Stop,
  with packet/authentication diagnostics rather than a ten-second click deadline.
  Windows learns the phone's source address only after authenticated pairing.
- Receivers first calibrate their output queues. Windows automatically budgets
  one common lead for the whole group before a five-second warmup. The default
  minimum is 40 ms; the chosen lead still covers measured device/capture needs
  plus a 30 ms reserve. It is not a 40 ms acoustic-latency promise.
  Optional local Windows playback requires a virtual capture route and a distinct
  physical output. Without that route, Windows' original speaker remains undelayed.
- Use a trusted LAN without client isolation. No firewall/interface settings are
  changed automatically. Never publish the setup code or expose UDP ports online.

The Windows executable is unsigned and Android is a test build. Verify source and
checksums; do not bypass security warnings for an untrusted download. Real-device
capture, playback and acoustic synchronization still require validation.

Instructions: [Windows](apps/desktop/README.md), [Android](apps/android/README.md),
[Windows speaker routing](docs/WINDOWS_LOCAL_PLAYBACK.md),
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
- WASAPI shared-loopback capture and optional synchronized local playback through
  a distinct explicitly chosen endpoint, with requested 48 kHz conversion,
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
Local speaker tests compare nonconstant PCM against a remote renderer at the same
virtual DAC times, and cover calibration, invalid clocks, bounded queues and route
validation. These do not execute the physical audio device.
Live-availability regressions include capture-block completion, network transit
and different receiver output queues, including the prior all-concealed failure
when a 100 ms lead was shorter than the output write-ahead requirement.
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

## npm launcher preview

A dependency-free Windows x64 launcher is in [packages/npm](packages/npm/README.md).
A verified local tarball supports `npx --package ./roomsync-0.1.5.tgz roomsync start`
or a local global install followed by `roomsync start`. It opens the existing
localhost setup page; it does not begin sharing audio automatically.

**Not published to npm.** Plain `npx roomsync start` is the intended future command,
not an available verified release yet. The package remains private/unlicensed until
its owner explicitly chooses publication, name, access and licensing. No install
hooks, runtime downloads or npm credentials were added.
