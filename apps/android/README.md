# RoomSync Android receiver 0.1.3

An installable Android 8.0+ receiver using the shared Rust audio/timing core and
Noise-PSK encrypted UDP. The APK includes **arm64-v8a** and **x86_64** libraries.
32-bit-only ARM phones are not supported by this preview.

**Android 0.1.3 works with Windows 0.1.2 or 0.1.3.** For this pairing fix, you
can update only Android; the matching Windows 0.1.3 download is optional. Builds
older than 0.1.2 need both apps updated. Interactive pairing uses the unchanged v3
handshake and an authenticated playback timeline negotiated after pairing.
The old five-second Connect window has been removed; either app can start first.
The Android package is versionCode 4, signed with the same project debug certificate
as 0.1.0/0.1.1/0.1.2, so the supplied update can install over that version without uninstalling.

0.1.3 updates pairing retry handling around handshake phase deadlines. Android
playback, output calibration, and focus handling are unchanged from 0.1.2.

## First connection

1. Put Windows and the phone on the same trusted Wi-Fi/LAN. Open the new Windows
   RoomSync control page. It supplies the Windows IP and ports automatically; if
   it lists multiple network adapters, select the one on the phone's LAN.
2. On Android tap **Scan Windows QR code**. Grant camera permission if you want
   scanning, then point at the code shown by Windows. Images are decoded on the
   phone, never saved or uploaded. Alternatively use **Paste setup code**.
3. The code automatically fills the Windows host IP, host port, receiver port and
   key. **Do not type the phone's IP into Windows:** the host learns it only after
   authenticating the receiver. Android displays its own active IPv4 addresses
   automatically for troubleshooting.
4. Select the desired output channel, then tap **Connect & play**. Start Windows
   playback whenever ready. Pairing waits and retries until you stop it. After all
   expected receivers pair, each plays calibration silence and measures its
   real output queue. Windows selects one common presentation lead that includes
   the slowest output, capture batching and a network margin, then assigns a
   future timeline with a five-second warmup. There is no rushed Connect countdown.
5. Keep the Windows source output inaudible or use headphones there: its original
   sound is not automatically muted or delayed, so it can echo against phones.
   Start ordinary unprotected music at modest volume.

Use **Stop** or the notification Stop action to stop receiving. **Retry saved
pairing** reconnects without retyping the key. Android retains the pairing only in
process memory, never on disk. **Forget pairing** clears it explicitly; Android
terminating the process also removes it. Closing a task may leave its process
cached, so use Forget if you want to remove the saved pairing immediately. For a
new key or changed host, scan/import again.

Manual entry remains available under **Enter or edit settings manually**. The
first receiver normally uses host port 5000 and phone port 5001; additional phones
use their own host port shown by Windows. Left and Right duplicate the selected
channel to both phone output channels; Mono averages the stereo pair.

## If it is still waiting

Use **Copy diagnostics (no key)** on Android and the Windows status. Diagnostics
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
wall-clock deadline is used. Before the playback timeline is selected, the actual
silence-filled AudioTrack queue is calibrated over at least 500 ms of advancing
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

Permissions: Internet (LAN sockets), foreground media playback, notifications,
playback wake lock, and optional camera for QR scanning. No microphone, contacts,
location, storage, account, analytics, cloud relay or private backend. The window
protects setup secrets from screenshots and Recents; use Copy diagnostics instead.

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
The Linux x86_64 build uses aapt2, D8, zipalign and apksigner directly, without
Gradle/AndroidX. `scripts/fetch-deps.sh` obtains the pinned ZXing JAR from Maven
Central and checks its SHA-256. Rust dependencies use `native/Cargo.lock`.
The output is `apps/android/build/RoomSync-debug.apk`; the script verifies its
signature and 16 KiB native-library ZIP alignment and prints SHA-256. Preserve the
local debug keystore between updates. It is excluded from source control.

Tests cover Rust formatting/clippy, actual encrypted loopback through clock lock,
post-pairing timeline ACK and decoded PCM rendering, delayed/unstarted pairing,
cancel/rebind, JVM timestamp regressions, actual JNI start/render/cancel/stop,
strict setup parsing/memory forgetting and offline QR decode from camera-format
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
source-availability failure, checks negotiated 200/250 ms leads, and rejects an
unsupported output before playback. This optional wall-clock test can fail under
cloud scheduling overload and is not a physical audio-quality certification.
The smoke retains a >95% nonzero-sample threshold for supported live schedules;
record its actual concealment and lateness, not just its exit code.

Platform references: [AudioTrack](https://developer.android.com/reference/android/media/AudioTrack),
[AudioTimestamp](https://developer.android.com/reference/android/media/AudioTimestamp),
[foreground playback services](https://developer.android.com/develop/background-work/services/fgs/service-types#media),
[ZXing](https://github.com/zxing/zxing/tree/zxing-3.5.4).
