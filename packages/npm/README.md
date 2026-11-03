# RoomSync npm launcher (unpublished preview)

A small, dependency-free launcher for the **Windows x64 RoomSync 0.1.5 host**.
It bundles the native executable and opens its local setup page. No Rust compiler,
extra runtime download, install script or paid service is needed. The launcher
does not install a virtual driver; optional synchronized Windows speakers require
one to be installed and routed separately by the user.
Requires Node.js 20+ and Windows 10 version 1703 or newer with x64 Node.js.
The Android receiver app is still installed separately. Android **0.1.4** remains
wire-compatible with Windows 0.1.5; no Android update is needed for this feature.

**This package has not been published to npm.** Do not run plain `npx roomsync`
until a release and its owner are confirmed; that command resolves a registry
package name, not this private source repository. The proposed name is not reserved.
The source package is deliberately `private: true` and `UNLICENSED` pending a
publication/name/license decision. Third-party notices are included separately.

## Use the preview tarball

After obtaining the verified `roomsync-0.1.5.tgz`, in **Windows PowerShell**:

```powershell
# Run directly from the local tarball, without a global installation:
npx --package .\roomsync-0.1.5.tgz roomsync start

# Or install the local package, then use the short command:
npm install --global .\roomsync-0.1.5.tgz
roomsync start
```

After an explicitly authorized public release, the intended equivalent is
`npx roomsync start` or `npm install --global roomsync` followed by `roomsync start`.
If a scoped package name is chosen instead, its installed command can still be
`roomsync`; the `npx`/install package name would change.

## Commands

```text
roomsync start               Open the local control page
roomsync start --no-browser  Start controls and print their address
roomsync devices             List Windows playback endpoints
roomsync doctor              Verify platform and packaged-host integrity
roomsync --version
roomsync --help
```

Keep the terminal open. Ctrl+C reaches the native host's stop handler. The local
page binds to a random `127.0.0.1` port; opening it does not start sharing audio.
Create a private setup code, connect the Android receiver and choose **Start
listening** in the page. No firewall or audio-routing settings change automatically.

macOS, Linux, WSL, Windows 32-bit and native ARM64 Node.js are not supported.
Help/version and package inspection still work there, but no host is launched.
Use Windows PowerShell/Command Prompt, not a WSL shell.

In default phone-only capture mode, original Windows audio is not muted or delayed.
Keep it inaudible in the listening room, for example through headphones, to avoid
echo. For synchronized Windows speakers, use the explicit routing below. This is an
unsigned preview, not a claim of zero latency or physically measured speaker
synchronization. The launcher does not validate audio hardware; `doctor` verifies
only package integrity/platform support.

## Optional synchronized Windows speakers

This opt-in preview sends the same timestamped PCM and common presentation
timeline to Windows speakers and Android. It does not retroactively delay audio
already playing through the captured physical endpoint.

1. Separately install a virtual cable from its official provider, such as
   [VB-Audio's VB-CABLE](https://vb-audio.com/Cable/), following its driver,
   administrator/restart and license instructions. No driver is bundled here.
2. Route the source applications you intend to share to the cable's **playback**
   endpoint, normally **CABLE Input**. RoomSync captures that output by loopback;
   the recording endpoint **CABLE Output** is not the capture selection.
3. In RoomSync Advanced settings, select that explicit virtual capture source,
   enable local speaker playback, and select a different physical speaker output.
   Review and confirm the route. RoomSync will not change Windows/app routing.
4. Do not enable Windows **Listen to this device** or another monitor/mixer path
   that duplicates the source or routes speaker output back into the cable.
5. Pair the Android receivers and start the group. The local output is measured
   while silent and included in the same budget as the phones. Keep the source
   and speaker selection unchanged during playback.
6. Stop the group before changing devices. Restore your usual app/Windows output
   afterward if you changed it, otherwise audio routed into the cable may be silent.

Same-endpoint routes are rejected. Strict queue and clock checks can reject
unsupported outputs or stop a session instead of silently shifting its timeline;
guarded local playback requires at least two engine periods of actual shared
buffer capacity. This remains a cloud-built preview, not physically verified
speaker synchronization. Bluetooth/DSP can add unmeasured delay. Use built-in or
wired speakers for initial testing.

## Safety and packaging

- The complete Windows executable is in the package. No `postinstall`, dynamic
  downloader, PowerShell script, shell command or PATH-selected executable runs.
- `start` maps only to the native `web` command. `--no-browser` maps to
  `web-headless`; `devices` maps to `list-devices`. Unknown options fail explicitly.
- SHA256, architecture, package/manifest version consistency and manifest fields
  are checked before execution. The manifest version is release metadata, not a
  runtime query of the executable; CI checks the native source version matches.
  This detects accidental modification/mismatched packages, **not** a compromise
  of both the package binary and manifest. The preview is not code-signed.
- No telemetry or automatic updater is added. No secret is stored in this package.
  The native app's private pairing and local-controller protections remain intact.
- npm's tarball integrity covers distribution. Verify the supplied tarball checksum
  and only install a release from the project's confirmed owner.

## Build a package from source (maintainers)

The release target is **x86_64-pc-windows-gnu**, matching the bundled preview and
the dependency-notice inventory. The default Windows MSVC target is not a drop-in
replacement: it may introduce different notice entries and VC runtime dependencies.
On Linux with the official distribution's MinGW cross-linker installed:

```sh
rustup target add x86_64-pc-windows-gnu
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
cargo build --release --locked --target x86_64-pc-windows-gnu --manifest-path apps/desktop/Cargo.toml
binary=apps/desktop/target/x86_64-pc-windows-gnu/release/roomsync-desktop.exe
hash=$(sha256sum "$binary" | cut -d' ' -f1)
node packages/npm/scripts/stage-native.cjs --binary "$binary" --notices docs/THIRD_PARTY_NOTICES.txt --source-commit "$(git rev-parse HEAD)" --expected-sha256 "$hash"
npm --prefix packages/npm test
mkdir -p dist
npm pack ./packages/npm --pack-destination dist
```

The stage command accepts a known,
locally built or independently verified Windows x64 executable; it never fetches
one. The manifest is written last and `prepack` refuses an incomplete or mismatched
bundle. Existing release inputs require their published/previously verified binary
digest, not a guessed digest. Keep npm/native versions in sync. If registry
dependencies change, regenerate the upstream notice inventory rather than merely
replacing its lock fingerprint.

Cloud packaging can use the existing verified Windows release without changing
its native audio implementation. Native builds/tests, JavaScript wrapper tests,
real tarball installation and physical hardware testing are separate checks;
passing one does not imply the others passed.

The repository's npm workflow tests the wrapper on Linux and Windows, builds the
Windows GNU host, audits DLL imports, stages the bundle and uploads a tarball artifact.
A separate Windows job installs the artifact and invokes native `--help` without
starting capture. These future CI jobs have not run merely because this file exists.
It deliberately
contains **no publish step, npm credentials, or auto-release**. Publication requires
a separate owner/account, name, access, licensing and release decision.

References: [npm bin/files metadata](https://docs.npmjs.com/cli/v11/configuring-npm/package-json/),
[Node child-process execution](https://nodejs.org/api/child_process.html).
