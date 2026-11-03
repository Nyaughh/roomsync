# RoomSync Android receiver

An installable Android 8.0+ receiver, not a WebView or UI mockup. It uses the same
Rust timing core and authenticated Noise-PSK UDP protocol as the Windows host.
The APK includes **arm64-v8a** and **x86_64** native libraries. 32-bit-only ARM
phones are not supported by this preview.

**Preview, cloud-built and not tested on physical Android/Windows audio hardware.**
Host-side tests verify encryption, clock lock, decoded PCM rendering, JNI lifetime,
and timestamp projection logic. They do not prove audible end-to-end synchronization,
actual device latency, stability on a particular phone, or call/video compatibility.

## Windows → Android first run

1. Install the supplied `RoomSync-debug.apk` on your Android phone. It is a debug
   build, signed with a project-local test certificate, not a Play Store release.
   Only install it if you trust the source and artifact checksum. If replacing a
   build signed with a different debug key, Android requires uninstalling that
   previous build first.
2. Connect phone and Windows to the same trusted LAN. Find the phone's Wi-Fi IPv4
   address in Android Wi-Fi settings. Guest/client-isolated networks will not work.
3. On Windows, launch `Start-RoomSync.cmd`, enter the phone IP, and privately reveal
   the generated pairing key when ready. The wizard prints the Windows IP, host
   port and receiver port. Windows' original sound is not delayed or muted, so use
   an inaudible output/headphones there to avoid an echo against the phone.
4. In RoomSync enter **Windows host IP**, **host UDP port** (first phone: `5000`),
   **this phone's UDP port** (`5001`), and the **64-character hexadecimal pairing
   key**. Select Stereo, Left, Right or Mono. Left/Right duplicate that channel to
   both of the phone's output channels.
5. Start the host by pressing Enter in Windows. Then tap **Connect & play** on the
   phone within the displayed pairing window. Allow the host's 15-second clock
   warmup. Grant notification permission if you want the persistent Stop control.
   Music continues through a media-playback foreground service while the screen is
   off; microphone permission is neither requested nor used.
6. Tap **Stop**, use the notification's Stop action, or close the task to end the
   phone receiver. Ctrl+C ends the Windows host. After an output route change,
   audio-focus interruption, timestamp failure, peer timeout or output underrun,
   restart the host and reconnect. Reconnection waits for the previous receiver
   thread to finish, up to five seconds during an unfinished handshake.

For multiple phones use each phone's own Wi-Fi IP, receiver port `5001`, and the
**specific consecutive host port printed by the wizard** (`5000`, `5001`, etc.).
This is fixed-peer LAN audio. There is no discovery, public relay,
unauthenticated fallback, or automatic firewall modification.

The key authenticates possession, not a person's identity. Share it privately;
never include it in screenshots or a repository. Android clears its entry after
starting, does not save it to disk, disables backups/autofill for it, and protects
the app window from screenshots. The Windows host owns key generation/storage.

## Timing and safe failure behavior

- Java AudioTrack plays 48 kHz stereo float PCM. Its advancing `AudioTimestamp`
  frame/time observations are projected to the next output frame, using the same
  `CLOCK_MONOTONIC` domain as Rust network clock exchanges.
- Shared Rust clock synchronization, bounded jitter buffer, sample interpolation,
  independent DAC drift estimation and channel mapping determine actual output.
  There is no second incompatible Java clock or wire protocol.
- No audio is rendered from a guessed wall-clock deadline. Playback begins with
  silence until valid advancing hardware timestamps and a fresh host-clock lock
  are available. Missing/unusable output timing times out after five seconds.
- Host clock loss, route changes, lost audio focus and output underruns stop this
  preview rather than claiming the timing is still correct. Network loss is
  concealed with the shared bounded fade-to-silence renderer.
- A dedicated network thread handles authenticated packets. The AudioTrack writer
  uses a short shared Rust state lock, never network I/O. Render batches are bounded
  to 960 frames and storage is bounded by the shared core.
- Status fields are observed counters and estimator values. Network-clock
  uncertainty is **not measured acoustic synchronization error**. Bluetooth,
  wireless speaker buffering, DSP and room propagation may add unmeasured delay.
  Prefer the built-in speaker or a wired output for the first test.

## Build in Linux / cloud

Install a JDK 17+, Rust, and the following official Android SDK components under
Google's [SDK terms](https://developer.android.com/studio/terms):

- Android SDK Platform 35
- Android SDK Build-Tools 35.0.0
- Android NDK 27.2.12479018 (r27c)

With a standard SDK layout:

```sh
rustup target add aarch64-linux-android x86_64-linux-android
export ANDROID_HOME=/path/to/android-sdk
./apps/android/test.sh
./apps/android/build.sh
```

Optional overrides: `ANDROID_NDK_HOME`, `ANDROID_BUILD_TOOLS`,
`ROOMSYNC_DEBUG_KEYSTORE`. The current script builds on Linux x86_64. It uses the
SDK's aapt2, D8, zipalign and apksigner directly, avoiding Gradle/Maven/AndroidX
build dependencies. Rust dependency versions are locked in `native/Cargo.lock`.
It creates `apps/android/build/RoomSync-debug.apk`, verifies its signature and
16 KiB native-library ZIP alignment, and prints its SHA-256 hash. Native ELF load
segments use 16 KiB alignment as well.

`test.sh` runs Rust format/clippy/tests, JVM timestamp regressions, and a real JNI
load/start/render/cancel/stop test on the Linux host. The encrypted-loopback Rust
test establishes the actual transport, locks clocks and renders decoded PCM.
Android hardware, foreground-service behavior, app installation/launch and
physical audio performance still require device validation.

No private backend, telemetry, microphone, contacts, location or storage permission
is used. The app requests Internet (LAN sockets), foreground media playback,
notifications, and a wake lock scoped to active playback. No battery-optimization
exception is requested. Device-specific power policies may still interrupt audio.

## Sources for platform behavior

- [AudioTrack timestamps and streaming playback](https://developer.android.com/reference/android/media/AudioTrack)
- [AudioTimestamp timebase and 32-bit frame position](https://developer.android.com/reference/android/media/AudioTimestamp)
- [Media playback foreground service requirements](https://developer.android.com/develop/background-work/services/fgs/service-types#media)
