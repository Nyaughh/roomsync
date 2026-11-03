# RoomSync npm launcher (unpublished preview)

A small, dependency-free launcher for the **Windows x64 RoomSync 0.1.4 host**.
It bundles the native executable and opens its local setup page. No Rust compiler,
extra runtime download, install script, virtual driver or paid service is needed.
Requires Node.js 20+ and Windows 10 version 1703 or newer with x64 Node.js.
The Android receiver app is still installed separately.

**This package has not been published to npm.** Do not run plain `npx roomsync`
until a release and its owner are confirmed; that command resolves a registry
package name, not this private source repository. The proposed name is not reserved.
The source package is deliberately `private: true` and `UNLICENSED` pending a
publication/name/license decision. Third-party notices are included separately.

## Use the preview tarball

After obtaining the verified `roomsync-0.1.4.tgz`, in **Windows PowerShell**:

```powershell
# Run directly from the local tarball, without a global installation:
npx --package .\roomsync-0.1.4.tgz roomsync start

# Or install the local package, then use the short command:
npm install --global .\roomsync-0.1.4.tgz
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

The original Windows audio is not muted or delayed. Keep it inaudible in the
listening room, for example through headphones, to avoid echo. This is an
unsigned preview, not a claim of zero latency or physically measured speaker
synchronization. The launcher does not validate audio hardware; `doctor` verifies
only package integrity/platform support.

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
