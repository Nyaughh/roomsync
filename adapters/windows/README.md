# RoomSync Windows audio adapter

Real Windows WASAPI system-output capture and shared-mode speaker playback. This crate is independent of the portable core workspace. **It has been cross-compiled/type-checked from Linux, not run against Windows audio hardware.** Do not treat it as a verified consumer release or as proof of acoustic synchronization.

## Implemented

- Captures the selected **render endpoint's complete output mix**, not the microphone, using WASAPI loopback.
- Requests and negotiates **48,000 Hz, stereo, float32** capture with `AUTOCONVERTPCM` and `SRC_DEFAULT_QUALITY`. Windows performs sample-rate/format/channel conversion. Unsupported negotiation is an error, never mislabeled native PCM.
- Requests stereo float32 playback at **48,000 Hz** too, with Windows high-quality shared-mode sample-rate/format/channel conversion to the actual endpoint mix. The core need not downsample to 44.1 kHz; both negotiated stream rate and native mix rate are exposed.
- Uses the documented shared/event zero-duration initialization; Windows selects the buffer. `max_buffer_duration_ms` only rejects an excessive returned size, not a promise of a requested latency.
- Dedicated COM-initialized audio threads, event-driven waits, optional MMCSS `Audio` scheduling, no socket I/O, no callback mutex, and preallocated sample storage.
- Bounded lock-free capture queue and reusable buffer pool. Slow consumers cause counted packet drops and a discontinuity on the next delivered packet, never unbounded memory or audio-thread blocking. Packet ownership includes the buffer until it is dropped.
- WASAPI capture timestamp and device-position metadata, invalid timestamp handling, silence/discontinuity flags, audio clock observations, render starvation counters, and a signed calibration correction.
- Explicit stop/join, RAII stream teardown and capture-buffer release, recoverable device-loss errors. Never silently switches output devices or restarts an old synchronization epoch.

Requires Windows 10 version 1703 or newer for native event-driven loopback. No virtual driver, admin installation, background service, credentials, or paid service. No network socket is opened by this crate.

## Build and local diagnostic

From the repository root, on a Windows machine with Rust and the Visual Studio C++ build tools:

```powershell
cargo test --manifest-path adapters/windows/Cargo.toml
cargo build --release --manifest-path adapters/windows/Cargo.toml --example audio_probe
.\adapters\windows\target\release\examples\audio_probe.exe devices
.\adapters\windows\target\release\examples\audio_probe.exe capture 10
# Explicitly requests a quiet 440 Hz tone. Lower physical volume first.
.\adapters\windows\target\release\examples\audio_probe.exe tone 3
```

The optional final argument is a render endpoint ID from `devices`. These probes do not save or transmit audio. Capture reports statistics and peak amplitude only. `tone` plays at -30 dBFS; no loopback-to-output echo path is created. Silence often produces no loopback packets, so a successful stream with zero packets while nothing is playing is normal.

Linux/cloud checks:

```sh
cargo test --manifest-path adapters/windows/Cargo.toml
cargo clippy --manifest-path adapters/windows/Cargo.toml --all-targets -- -D warnings
rustup target add x86_64-pc-windows-gnu
cargo check --manifest-path adapters/windows/Cargo.toml --target x86_64-pc-windows-gnu --all-targets
cargo clippy --manifest-path adapters/windows/Cargo.toml --target x86_64-pc-windows-gnu --all-targets -- -D warnings
```

Cross-checking alone does not link an EXE or exercise devices. On Linux, installing the official distribution MinGW cross-linker permits an actual binary build:

```sh
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
cargo build --release --manifest-path adapters/windows/Cargo.toml --target x86_64-pc-windows-gnu --example audio_probe
```

This real release build also succeeded in the cloud using GCC MinGW 14.2. The result is `adapters/windows/target/x86_64-pc-windows-gnu/release/examples/audio_probe.exe`, a Windows x64 PE executable. Its import table contains only Windows system DLLs, with no separate MinGW runtime DLL required. **It has not been executed on Windows or tested with audio hardware.**

## Integration boundary

```rust,ignore
let capture = CaptureStream::start(CaptureConfig::default())?;
while let Some(packet) = capture.try_recv() {
    // Transport/control thread, NOT audio callback:
    // 1. Reject/reset on invalid timestamp or a discontinuity.
    // 2. Reblock packet.frames() into the core's 120-frame PCM16 packets.
    // 3. Map first-frame QPC to the authenticated host timeline.
    // 4. Preserve timestamp gaps when loopback emits no data. Do not concatenate
    //    separated audio as if the gap never existed.
    // 5. Drop packet promptly to return its storage.
}

let render = RenderStream::start(RenderConfig::default(), move |timing, out| {
    // Own Renderer and a bounded network-to-render handoff in this closure.
    // Consume only a bounded amount of ready work per callback.
    // renderer.render(timing.estimated_dac_start_ns, timing.sample_rate, clock, out)
    // Never send or receive sockets, allocate, log, or wait on a mutex here.
    RenderFill::Audio
})?;
```

`out` is pre-zeroed, stereo, at most 8,000 frames. Returning `RenderFill::Silence` ignores any written samples and sets WASAPI's silent flag. Non-finite samples are silenced and finite samples clipped before playback. A complete silence block is preferable to inventing a deadline when synchronization is not locked.

`qpc_now_ns()` uses QueryPerformanceCounter with QueryPerformanceFrequency. It shares the **same machine-local QPC epoch** as the capture and audio-clock timestamps; WASAPI returns 100 ns units and the adapter converts them once to ns. Never combine these with an arbitrary `Instant` epoch or wall clock. Different machines still require the transport's clock synchronization.

## Timing and recovery limits

`estimated_dac_start_ns` projects the first frame of the next write from the correlated audio-clock position/QPC pair and cumulative submitted frame count. It divides by the audio clock's actual frequency, rather than assuming its units are PCM frames. A stale write cursor stops with a reopen-required error; engine padding cannot safely reconstruct unknown in-flight device latency. Invalid/regressing/stale clock observations also stop. The control layer must reopen and establish a fresh synchronization epoch.

This remains an **estimate**, not a measured acoustic deadline. `maximum_stream_latency_ns` reports WASAPI's maximum stream latency separately; it is not automatically added to the clock projection because that could double count latency. Bluetooth, driver buffering, speaker DSP and physical propagation can add unknown delay. Set `extra_output_delay_ns` only from a deliberate per-device calibration; a positive correction means the sound arrives later. Do not promise sample-accurate multi-speaker sync without measuring the actual outputs.

Check `is_finished()` from the control thread. `stop()` joins and returns the terminal error (if any); dropping a stream requests stop and joins, but necessarily discards an error. Device unplug, audio-service failure or invalidation is surfaced as `ReopenRequired`. Re-enumerate/open after the condition is resolved and start a fresh synchronization epoch. The default endpoint is chosen at startup and then pinned; a later default-device change does not move an active stream automatically. Capture event timeouts during silence are not fatal. Playback with no device events for two seconds fails explicitly.

## Host echo and output routing

Loopback observes what is already playing on the source endpoint. It cannot retroactively delay that original output. Playing the delayed receiver output on the **same captured endpoint creates a feedback/echo loop**. Even with separate endpoints, an audible undelayed host speaker plus delayed receivers creates echo.

This crate deliberately never mutes the captured endpoint, alters user volume, installs a virtual cable, or claims muting preserves capture. A full synchronized host-speaker experience needs an explicitly chosen routing arrangement (for example a separate capture-only output and a different receiver output) and device testing. Until that exists, keep the host's original sound physically inaudible, such as by capturing a dedicated unused/headphone endpoint whose real-world sound is not in the listening room. Changing routing or installing a driver is a separate user-controlled step.

The captured mix may include notifications and calls. A transport integration must require deliberate sharing and authenticated/encrypted peer setup; it must not default to an open LAN stream.

## Safety-relevant implementation detail

The `wasapi` 0.26 high-level `read_from_device` helper creates a slice from the returned capture pointer before checking `SILENT`. WASAPI allows clients to treat a silent packet as zeros without reading its pointer. This adapter therefore calls the official `IAudioCaptureClient::GetBuffer` directly, never reads a silent packet pointer, and releases every acquired nonempty packet once with an RAII guard. The wrapper is still used for endpoint/lifecycle operations and render writes.

## Primary references

- [Microsoft: loopback recording and Windows 10 1703 event support](https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording)
- [Microsoft: IAudioCaptureClient::GetBuffer, flags and QPC units](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudiocaptureclient-getbuffer)
- [Microsoft: stream flags, automatic conversion and SRC](https://learn.microsoft.com/en-us/windows/win32/coreaudio/audclnt-streamflags-xxx-constants)
- [Microsoft: IAudioClock::GetPosition](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclock-getposition)
- [Microsoft: IAudioClock::GetFrequency](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclock-getfrequency)
- [Microsoft: IAudioClient::GetStreamLatency](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-getstreamlatency)
- [Microsoft: IAudioClient::Initialize, including zero shared-event duration](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-initialize)
- [Microsoft: rendering a stream, including priming before Start](https://learn.microsoft.com/en-us/windows/win32/coreaudio/rendering-a-stream)
- [wasapi 0.26 API and source](https://docs.rs/wasapi/0.26.0/wasapi/)
- [crossbeam-queue ArrayQueue](https://docs.rs/crossbeam-queue/latest/crossbeam_queue/struct.ArrayQueue.html)

## Hardware validation still required

Run capture with active audio and silent sources at native 44.1/48/96 kHz; confirm stream stays 48 kHz and timestamps/gaps make sense. Test selected versus default endpoint, unplug/replug, service restart, output format changes, sleep/resume, full/held capture queues, delayed callback starvation, and rapid start/stop. Measure multi-device impulses with a common recording source and verify calibration, drift and reconnect behavior before claiming room sync. Bluetooth endpoints especially need measured latency and jitter.
