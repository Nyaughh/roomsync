# RoomSync Windows desktop preview 0.1.1

A native Windows audio host with a small local browser control page. The browser
handles setup and status only; capture, encryption and audio timing remain native
Rust/WASAPI. **Windows audio and acoustic synchronization still need physical-device
validation.** This unsigned preview is not a finished consumer release.

## Windows → Android: simpler setup

Update **both** apps to 0.1.1. The new authenticated pairing flow is not compatible
with the old timed-pairing UI.

1. Extract the Windows ZIP. Open `Start-RoomSync.cmd`; keep its console open. It
   opens controls on this computer at a random `127.0.0.1` port. If a browser does
   not open, use the local address printed in the console. The address contains no
   pairing secret.
2. Windows fills a dropdown from its actual LAN adapters. Wi-Fi/Ethernet names are
   preferred over recognizable VPN/virtual adapters. If several are available,
   choose the network your phone uses. No phone IP needs typing and no interface
   settings are changed.
3. Choose the receiver count, then **Create private setup code**. Open **Show private
   code / QR**. The QR is generated entirely on Windows, with no external service.
4. In the updated Android app, scan the QR or paste the **whole setup code**. Host
   IP, ports and key fill automatically. The code is plain `RS1|...` text, not a
   web link. Share it only with your receivers; do not post a screenshot.
5. Click **Start listening** in Windows and **Connect** in Android in either order.
   There is **no ten-second deadline**. Windows learns the phone's IP/port only
   after authenticated pairing. Stop cancels waiting. Already-paired phones stay
   connected while the rest of the configured group joins.
6. After all phones pair, Windows confirms one shared timeline and allows a
   five-second clock warmup. Play ordinary unprotected music. Windows' original
   output is not muted or delayed, so keep it inaudible, for example with headphones,
   to avoid echo relative to the phones.
7. **Stop** closes the audio stream and UDP listeners. Start listening again uses
   the same in-memory setup code. Creating a new setup code replaces its secret;
   import the new code on Android. Closing the controller discards its key.

Each receiver gets its own code selected from the receiver dropdown. Host ports
are consecutive (default 5000, 5001, ...); Android's local port defaults to 5001.
Up to eight configured receivers share one timeline. Actual capacity is not measured.
A receiver leaving after playback starts stops the group in this preview.

## Connection status is evidence, not a guess

The page distinguishes:

- No peer UDP traffic yet: check both devices use the same LAN, the selected host
  adapter, guest/client isolation and the app's private-network firewall permission.
- UDP reached Windows but authentication failed: recopy or rescan the complete code.
- Setup code verified, awaiting encrypted confirmation: traffic reached both ends,
  but the final handshake has not completed.
- Incompatible pairing version: update both apps.
- Authenticated and waiting for the group / shared timeline confirmed.

A generic timeout cannot by itself prove a firewall, Wi-Fi or key problem. The new
flow keeps waiting until Stop and reports observed packet/authentication counts.
No firewall rules, port forwarding or security settings are changed automatically.
Do not expose this preview's UDP ports to the internet. On a Windows firewall
prompt, permit only the intended private network after checking the executable.

## Local-controller security

The HTTP server binds only to 127.0.0.1 and checks its exact Host header. API calls
require a per-run random control token; modifying calls also require the exact
same Origin. It sends no CORS grants, rejects oversized/chunked/malformed requests,
uses bounded read/write deadlines, disables caching/framing and loads no remote
scripts. A pairing key is returned only by the explicit private-code action, never
ordinary status responses or logs. Clipboard copying requires a user click.

This protects against ordinary cross-site and DNS-rebinding attacks, not malicious
software already running under the same local user. Pairing authenticates possession
of a 256-bit secret, not an individual's identity. Keys are memory-only in the new
control-page flow. The old CLI's explicit key-file mode remains available separately.

## Commands and developers

```
roomsync-desktop.exe web
roomsync-desktop.exe web-headless
roomsync-desktop.exe list-devices
```

No arguments on Windows and the old `wizard` command both open the new controls.
`web-headless` serves without automatically opening a browser.

Explicit legacy `host --key ... --bind ... --peer ...` and `receive --key ...`
commands remain for developer tests; use the control page and updated Android app
for cancellable, automatic-address pairing. The old fixed-peer command does not
provide the new no-deadline discovery UI.

Build/test:

```
cargo test --manifest-path apps/desktop/Cargo.toml --all-targets --locked
cargo clippy --manifest-path apps/desktop/Cargo.toml --all-targets --locked -- -D warnings
cargo build --manifest-path apps/desktop/Cargo.toml --release --locked
```

For browser QA on a Linux cloud host, `web-ui-test` supplies an explicitly labeled
reserved synthetic address and allows setup/QR/security tests. It cannot start
network audio, pretend a phone connected, or claim audio playback. Normal Linux
`web-headless` also refuses real capture. Windows cross-linking checks buildability,
not physical WASAPI behavior.

The source uses 48kHz stereo PCM. Protected streams may be unavailable. Microphone
transport, call echo cancellation and video lip-sync are outside this preview.
