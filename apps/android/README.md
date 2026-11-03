# RoomSync Android receiver 0.1.6

A minimal Android 8.0+ receiver using the shared Rust audio/timing core and
Noise-PSK encrypted UDP. The APK supports **arm64-v8a** and **x86_64**.
32-bit-only ARM phones are not supported by this preview.

**Installing this update:** 0.1.6 uses the same signing certificate as the
previously distributed 0.1.4 preview, so it can install over that version without
uninstalling. The package is versionCode 6. Pre-0.1.4 previews used a different,
unavailable signing key: those must be uninstalled first, removing their app
data, before installing this APK. Nothing is uninstalled automatically.

This is a diagnostic update, not a verified cure for audio cutouts. It adds
bounded receive/processing timing counters without changing Android's playback
queue, clocks or shared timeline. Use Windows 0.1.6 for complementary host timing
counters. Windows 0.1.2 and newer remain compatible; the encrypted revision 3
protocol is unchanged. Windows 0.1.4 and newer include the measured-latency budget
improvements.

## First connection

1. Put Windows and the phone on the same trusted Wi-Fi/LAN. Open RoomSync on
   Windows and choose the network adapter on the phone's LAN if necessary.
2. On Android tap **Scan QR code** and scan the code shown by Windows. Camera
   permission is optional; **Paste setup code** is the alternative. Images are
   decoded locally and never saved or uploaded.
3. Tap **Connect**. Windows and the phone exchange their connection details
   automatically; you do not need to type the phone's IP address.
4. Each receiver measures its real output queue. Windows chooses one common
   presentation lead covering the slowest receiver, capture and a network margin,
   then schedules a shared future start. There is no rushed pairing countdown.
5. Start ordinary unprotected audio on Windows. Its original output is not
   automatically muted or delayed, so it can echo against the receiving phone.

The main screen shows one connection action and a short status. Tap **Disconnect**
to stop. **Connect** reuses the setup held in app memory. **Change computer** lets
you scan or paste a different setup.

**Settings** contains the audio channel, manual setup and **Forget computer**.
Settings cannot change a running audio session. Left and Right duplicate the
selected channel to both phone channels; Mono averages the stereo pair. Stereo is
the default. Manual ports and keys are not shown on the main screen.

Setup is kept only in process memory, never on disk. Forget clears it; Android
terminating the process also removes it. Closing the app may leave its process
cached, so use Forget to explicitly remove the setup.

## If it is still waiting

Open **Connection details** on Android and use **Copy details**, alongside the
Windows status. Diagnostics
show the actual configured host, UDP bind/selected route IP, elapsed wait, attempts,
HELLO sends, verified replies, confirmation sends and received/rejected packets.
After pairing they also show the measured next-write DAC horizon, calibrated
maximum, negotiated shared lead, real PCM peaks, source-frame position, and jitter
buffer accepted/late/too-far counts. Late-packet lag shows p95 over the last
256 late packets and the session maximum in source frames; it is not acoustic
synchronization error. Transport authentication rejection is separate
from late audio packets. Nonzero rendered PCM does not prove physical audibility.
They never include the pairing key or complete setup code.

- No incoming UDP establishes only that no reply has arrived. It does **not** prove
  a specific cause. Check both app versions, the Windows LAN IP, intended private-
  network firewall permission, guest/client-isolated Wi-Fi and VPN routing.
- Authentication rejection means traffic arrived but did not authenticate; scan a
  freshly generated setup code from the matching Windows instance.
- Version mismatch means both apps need updating together.
- A securely paired phone may wait while the other expected receivers connect.
  Clock exchange and authenticated keepalives continue during this wait.

No firewall, VPN, permission or operating-system security setting is changed by
this app. Do not expose the audio ports to the public internet. Pairing keys prove
possession, not a person's identity. Rotate a key if it reaches the wrong person.

## Privacy and implementation

The setup payload is bounded non-URL text:

```
RS1|192.168.1.8|5000|5001|<64 hexadecimal key characters>
```

The parser accepts outer copied whitespace but rejects internal whitespace,
unknown versions, extra fields, noncanonical numeric IPv4 addresses, unsupported
unicast destinations, invalid ports and keys not exactly 256 bits. Neither the
scanner nor parser opens URLs, invokes another app, performs DNS lookup or calls
an external QR service. The scanner uses pinned ZXing core 3.5.4, Apache-2.0;
license and notice are included in the APK assets and repository. Native Rust
dependency notices are copied from `docs/THIRD_PARTY_NOTICES.txt` into the APK
assets by the build script. Camera permission
is requested only after tapping Scan. A denied/unavailable camera leaves Paste
available. Camera resources are released on pause/cancel; decoding uses a single
worker, bounded frames, and a maximum five attempts per second.

Java AudioTrack plays 48 kHz stereo float PCM. Advancing AudioTimestamp frame/time
observations are projected onto the next output frame in the same CLOCK_MONOTONIC
domain used by Rust. Shared Rust clock synchronization, bounded jitter buffering,
interpolation and independent DAC drift estimation determine playback. No guessed
wall-clock deadline is used. Android keeps the original allocation as backoff capacity but initially requests
an effective two-block (20 ms) queue. The device may round or refuse that size.
During silence-only startup, underruns grow the queue by one 10 ms block within
capacity, resetting the timing measurements while retaining the original five-
second deadline. Readiness publication freezes the queue; later underruns still
require reconnecting. No claimed latency is inferred from requested buffer size.
Before the playback timeline is selected, the actual silence-filled AudioTrack queue is calibrated over at least 500 ms of advancing
hardware observations. Its maximum measured next-write horizon and 10 ms render
block are sent inside the authenticated session. A common lead is bounded to
300 ms by the existing 320 ms jitter window; unsupported outputs fail explicitly.
A sustained output-horizon increase greater than 10 ms requires reconnecting. Missing initial hardware timing times out after five
seconds; loss of an established timeline, route change, clock discontinuity,
permanent audio-focus loss or output underrun requires reconnecting. Short transient
focus loss mutes output for up to 30 seconds while clocks and current audio keep
advancing. Gain restores volume only on the still-valid live timeline; a ducking
request lowers volume without stopping. A longer interruption requires reconnecting.

Status contains actual counters and estimates. Network clock uncertainty is **not
measured acoustic synchronization error**. Bluetooth, device DSP and acoustic
propagation add unmeasured delay. Prefer built-in/wired speakers for initial tests.
Microphone transport, call handling and video lip-sync remain out of scope.

Connection details also records bounded receive-timing histories (p95 of the last
256 observations per metric and session maximum including startup): audio receive gaps, socket polling beyond
its requested 5 ms wait, work between polls, audio-insertion/render state-lock waits, and
render work. Estimated source age is calculated from the shared source schedule,
not a send timestamp: capture batching, host dispatch, transit, socket backlog and
receiver scheduling can all contribute. These counters diagnose stalls without
changing the clock, queue size or common playback timeline. Low clock uncertainty
and zero AudioTrack underruns do not guarantee complete audio: missing packets
are concealed on the continuing timeline, and high-RTT clock samples can be
rejected while a previously valid clock estimate remains usable.

Permissions: Internet (LAN sockets), foreground media playback, notifications,
playback wake lock, and optional camera for QR scanning. No microphone, contacts,
location, storage, account, analytics, cloud relay or private backend. The window
protects setup secrets from screenshots and Recents; use Copy details instead.

## Build and test in Linux/cloud

Install JDK 17+, Rust, and official Android SDK Platform 35, Build-Tools 35.0.0,
and NDK 27.2.12479018 under Google's [SDK terms](https://developer.android.com/studio/terms).

```sh
rustup target add aarch64-linux-android x86_64-linux-android
export ANDROID_HOME=/path/to/android-sdk
./apps/android/test.sh
ROOMSYNC_LIVE_TIMING=1 ./apps/android/test.sh # Optional real-time cloud/JNI smoke
./apps/android/build.sh
```

Overrides: `ANDROID_NDK_HOME`, `ANDROID_BUILD_TOOLS`, `ROOMSYNC_DEBUG_KEYSTORE`.
Set `ROOMSYNC_DEBUG_KEYSTORE` to the securely backed-up key for your existing
installation. A missing key now stops the build rather than silently replacing
the signing identity. `ROOMSYNC_UNSIGNED_ONLY=1` verifies compilation and packages
an unsigned APK. `ROOMSYNC_ALLOW_NEW_DEBUG_KEY=1` explicitly permits a new identity
for a fresh installation; it cannot update an APK signed with a different key.
The Linux x86_64 build uses aapt2, D8, zipalign and apksigner directly, without
Gradle/AndroidX. `scripts/fetch-deps.sh` obtains the pinned ZXing JAR from Maven
Central and checks its SHA-256. Rust dependencies use `native/Cargo.lock`.
The output is `apps/android/build/RoomSync-debug.apk`; the script verifies its
signature and 16 KiB native-library ZIP alignment and prints SHA-256. Preserve the
local debug keystore between updates. It is excluded from source control.

Tests cover Rust formatting/clippy, actual encrypted loopback through clock lock,
post-pairing timeline ACK and decoded PCM rendering, delayed/unstarted pairing,
cancel/rebind, JVM timestamp regressions, actual JNI start/render/cancel/stop,
plain-language UI states, queue growth/readiness freezing, publication deadline
and underrun races, strict setup parsing/memory forgetting and offline QR decode from camera-format
luminance. **Physical Android camera/lifecycle/audio and end-to-end acoustic sync
are not cloud-validated.** The user's real phone paired after VPN was paused. A subsequent silence failure
was reproduced in cloud when live capture's presentation lead was shorter than
the receiver's measured write-ahead horizon; this release negotiates that budget
before playback. Physical audio success still needs testing on the phone.

The required Android tests need only Linux, Rust and a JDK, not the Android SDK.
The desktop suite separately requires deterministic, zero-concealment live-source
scheduling and equal PCM across different hardware queue horizons. Set
`ROOMSYNC_LIVE_TIMING=1` to run the additional real JNI/encrypted-UDP smoke with
mock AudioTrack timestamps and the actual desktop packetizer/budget helper.
It measures Java callback and capture-send lateness, reproduces the old 100 ms
source-availability failure, checks negotiated 70/200/250 ms leads, and rejects an
unsupported output before playback. This optional wall-clock test can fail under
cloud scheduling overload and is not a physical audio-quality certification.
The smoke retains a >95% nonzero-sample threshold for supported live schedules;
record its actual concealment and lateness, not just its exit code.

Platform references: [AudioTrack](https://developer.android.com/reference/android/media/AudioTrack),
[AudioTimestamp](https://developer.android.com/reference/android/media/AudioTimestamp),
[AudioTrack effective buffer tuning](https://developer.android.com/reference/android/media/AudioTrack#PERFORMANCE_MODE_LOW_LATENCY),
[foreground playback services](https://developer.android.com/develop/background-work/services/fgs/service-types#media),
[ZXing](https://github.com/zxing/zxing/tree/zxing-3.5.4).
