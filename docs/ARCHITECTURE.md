# RoomSync architecture and honest capability boundary

RoomSync is an early native LAN-audio prototype. Core correctness and virtual
clock/network behavior can be checked in the cloud; real output skew, driver
latency, power consumption and acoustic behavior require physical hardware.
Zero latency is impossible. End-to-end delay and inter-speaker skew are separate
metrics. Video playback needs matching video delay; arbitrary video/call apps
cannot necessarily provide it. Call audio is receive-only here, not microphone
transport or echo cancellation.

## Data path

1. A platform host captures supported system playback with monotonic timestamps.
2. Host converts or requests 48 kHz stereo and packetizes 120-frame PCM16 blocks.
3. Each source frame is assigned a shared future presentation timeline. The host's
   own speaker must be delayed onto that timeline or silent, or it will echo.
4. Authenticated LAN transport protects content and replay-sensitive control.
5. Receivers estimate local-to-host clock offset/rate from four timestamps.
6. A fixed 128-packet (320 ms) reorder buffer accepts only its session/window.
7. A real-time callback renders fractional source positions from an estimated or
   measured DAC timestamp, accounting for clock drift and explicit output delay.

The Rust core has no third-party dependencies and forbids unsafe code. Packet
parsing, insertion, model updates and callback output use fixed storage. The
simulation executable does allocate offline event lists; it is not the callback.
Audio callbacks must not block, allocate, log, touch network sockets or acquire
contended locks. Adapters should transfer bounded records through a preallocated
SPSC queue and report overflow instead of growing memory or hiding dropouts.

## Timing controls

The renderer separately estimates the DAC sample clock from correlated presentation timestamps and frame counts, mutes until host-clock lock, mutes stale mappings, and slews small clock-model
corrections at up to 200 ppm, and reports/fades re-anchors above five milliseconds.
Drift correction uses inexpensive linear interpolation. This is suitable for
proving timeline control, not a claim of transparent high-fidelity resampling.
A production version needs measured frequency-response/THD tests and a bounded
band-limited resampler, especially for differing device sample rates. The core currently rejects output rates below 48 kHz instead of aliasing downsampled audio; Windows requests a 48 kHz stream and uses the OS conversion path.

Missing samples decay toward silence; recovery fades over 64 frames. This is
simple concealment, not recovery of lost music. Stereo mapping supports stereo,
left, right and a headroom-safe mono average. It does not synthesize true 5.1/7.1
surround from stereo. Different receiver speakers will also have different DSP,
frequency responses and acoustic propagation delays.

## Platform constraints and sources

- **Windows:** use WASAPI shared-mode loopback on a selected rendering endpoint.
  Event-driven loopback is supported on Windows 10 version 1703+. Protected audio
  can be unavailable. Capture does not itself silence or delay the host's existing
  speaker output. Microsoft: https://learn.microsoft.com/windows/win32/coreaudio/loopback-recording
- **Android receiver:** Oboe/AAudio callbacks, negotiated native rates, timestamp
  observation and buffer tuning are the intended path. Low-latency/exclusive mode
  is a request, not guaranteed hardware behavior. Google:
  https://developer.android.com/games/sdk/oboe/low-latency-audio
  and https://github.com/google/oboe/blob/main/docs/FullGuide.md
- **Apple receiver:** AVAudioEngine provides a native real-time audio graph;
  AVAudioSession route, interruptions and output latency require careful handling.
  Apple: https://developer.apple.com/documentation/avfaudio/avaudioengine
- No mobile app or universal macOS/Linux system-audio capture is implied by this
  core. Each adapter must advertise only operations actually implemented and
  hardware-tested. Bluetooth adds output buffering outside this core's control.

## Required pre-release gates

- Physical multi-device skew measurement (electrical loopback first, then acoustic),
  with p50/p95/p99 and confidence/uncertainty, not just averages.
- Drift/route-change, Wi-Fi roaming, sleep/resume, packet storms, loss bursts,
  disconnect/rejoin, background mobile playback and long soak tests.
- Secure pairing UX, bounded connection admission/rate limiting, independent
  cryptographic protocol review, key storage and rotation before broad exposure.
- Windows code signing and installer/uninstaller; Android release signing, physical-device lifecycle testing and store rules. iOS remains unimplemented.
- Playback-consent/recording indicators and an obvious stop control. Do not capture
  or relay microphone audio or protected audio without explicit supported scope.
