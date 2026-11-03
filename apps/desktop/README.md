# RoomSync Windows desktop prototype

A real Windows executable joining WASAPI capture/render, the Rust timing core and
authenticated UDP. **Cloud-built, not yet tested on physical Windows/Android
hardware.** It is unsigned and not a finished consumer product. Do not bypass a
security warning for an untrusted download; verify its source and SHA-256 manifest.

## First Windows → Android test

1. Extract the Windows package. Keep `Start-RoomSync.cmd` beside
   `roomsync-desktop.exe`. Install the separately supplied Android receiver.
2. Connect both devices to the same trusted Wi-Fi/LAN. Guest Wi-Fi/client isolation
   will not work. Find the phone's Wi-Fi IPv4 address in Android settings.
3. Use headphones or another inaudible Windows output for the source. The app does
   **not delay or mute Windows' original playback**, so audible host speakers can
   echo relative to the delayed phones. No endpoint settings are changed.
4. Double-click `Start-RoomSync.cmd`; enter the phone's IP address. The guided setup
   determines the Windows LAN address and prints the pairing settings.
5. Reveal the pairing key only when ready to copy it privately into Android. It is
   stored under your Windows user app-data folder. Never post it or include it in
   screenshots. The key file inherits the folder's Windows access permissions.
6. Fill Android's host address, host port, receiver port **5001**, key and channel.
   Do not connect yet. Press Enter in Windows, then Connect on Android within ten
   seconds. Playback waits for a 15-second shared clock warmup.
7. Play ordinary unprotected music through the selected/default Windows output at
   modest volume. Ctrl+C stops the host; Stop on Android stops that receiver.
   Restart after an output-route/device change or a failed peer.

Multiple phones: enter comma-separated addresses (up to eight). The host uses
consecutive ports **5000, 5001, ...**, one fixed-peer session per phone; each phone
uses receiver port 5001 on its own IP. The wizard prints each specific host port.
All share the same stream ID and presentation epoch. Capacity is hardware-unverified.
A phone leaving stops this initial host session rather than partially reconfiguring it.

If Windows asks for firewall access, permit only the intended private/home network
once you trust the executable. The app does not change firewall rules. Do not
expose its ports to the internet. Pairing keys authenticate possession, not a
person's identity; rotate a key if it is shared with the wrong party.

## CLI

```
roomsync-desktop.exe list-devices
roomsync-desktop.exe generate-key pairing.key
roomsync-desktop.exe host --key pairing.key --bind 192.168.1.10:5000 --peer 192.168.1.20:5001
roomsync-desktop.exe receive --key pairing.key --bind 192.168.1.20:5001 --host 192.168.1.10:5000 --channel left
```

Host accepts `--latency-ms 40..250` (default 100), `--capture-device ID`, repeated
`--peer IP:PORT`. Receiver accepts `--channel stereo|left|right|mono`,
`--output-device ID`, and `--output-delay-ms -20..20` for a **measured** correction.
The correction is bounded so it cannot exceed the entire minimum transport lead
or reorder window. Buffer settings are not measured acoustic latency.

Windows converts to 48 kHz stereo. Timestamped capture is normalized onto the host
clock grid with bounded drift interpolation; receivers independently estimate
network-clock and DAC-clock rate. Idle source gaps preserve the global timeline
and become silence. Invalid/backward timestamps and device/worker failures stop
the session. Protected streams may be unavailable. Microphone transport, echo
cancellation, video lip-sync and call-specific behavior are outside this version.

## Build and test

```
cargo test --manifest-path apps/desktop/Cargo.toml --all-targets --locked
cargo clippy --manifest-path apps/desktop/Cargo.toml --all-targets --locked -- -D warnings
cargo build --manifest-path apps/desktop/Cargo.toml --release --locked
```

Non-Windows builds run help, parser and packetizer tests; actual capture/playback
explicitly returns unsupported-platform errors. Windows cross-compilation checks
compilation and linking, not device behavior.
