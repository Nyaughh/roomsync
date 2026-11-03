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

Before the interactive host publishes a timeline, every authenticated receiver
runs its output silently and reports its calibrated maximum write-ahead horizon
and complete render-batch duration. The host adds the Windows capture-buffer /
stream-latency / engine-period allowance and 30 ms for network and clock variation.
It chooses one common lead, rounded upward to 10 ms. A user preference is only a
minimum. A requirement above 300 ms is rejected, leaving 20 ms inside the fixed
320 ms packet window rather than silently asking for source audio before capture.
The chosen lead is authenticated with the timeline and validated by receivers.
A changed calibrated output requirement means restarting the group.

The regression suite models when a captured block actually becomes available,
not just preloaded synthetic audio. It reproduces the former all-concealed output
at 100 ms source lead with a 150 ms receiver queue, then verifies the calibrated
path, including identical nonconstant PCM at shared DAC times across mixed queues.
This is virtual timing evidence, not an acoustic synchronization measurement.

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
- **Android receiver:** the current native Rust receiver is fed through JNI by a
  48 kHz float `AudioTrack`. `AudioTimestamp` correlates frame positions with the
  monotonic clock, including queued presentation time. Low-latency mode is a
  request, not guaranteed behavior; output queues are calibrated before playback.
  Google: https://developer.android.com/reference/android/media/AudioTrack
  and https://developer.android.com/reference/android/media/AudioTimestamp
- **Apple receiver:** AVAudioEngine provides a native real-time audio graph;
  AVAudioSession route, interruptions and output latency require careful handling.
  Apple: https://developer.apple.com/documentation/avfaudio/avaudioengine
- Android implementation lives in `apps/android`; iOS and universal macOS/Linux
  system capture remain unimplemented. Each adapter must advertise only operations
  actually implemented and distinguish build tests from hardware validation.
  Bluetooth adds output buffering outside this core's control.

## Host-speaker routing is not implemented

Endpoint loopback observes the mix already sent toward the physical output. It
does not withhold the original sound while RoomSync buffers the remote copies.
Default loopback is tapped before endpoint volume/mute, but muting that endpoint
also silences RoomSync's delayed replay on the same endpoint. Process loopback
can exclude RoomSync's process to prevent feedback; it does not provide a
suppress-original-playback option. Arbitrary system audio needs an independently
silent source route, such as a properly signed virtual endpoint, before both
host and phones can render the delayed stream. That routing/driver work is not
included in this preview. A source owned by RoomSync could be delayed directly,
but that would not cover arbitrary Spotify or system audio.

Microsoft references:
- https://learn.microsoft.com/en-us/windows/win32/api/audioclient/ne-audioclient-audclnt_streamoptions
- https://learn.microsoft.com/en-us/samples/microsoft/windows-classic-samples/applicationloopbackaudio-sample/
- https://learn.microsoft.com/en-us/samples/microsoft/windows-driver-samples/sysvad-virtual-audio-device-driver-sample/

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
