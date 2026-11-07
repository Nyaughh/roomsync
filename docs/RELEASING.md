# Releasing RoomSync

The Android download URL used by the CLI is:

https://github.com/Nyaughh/roomsync/releases/latest/download/RoomSync.apk

Every release marked **Latest** must include the signed APK under the exact asset
name `RoomSync.apk`, even if the release also updates the npm launcher. Publish a
normal GitHub release (the app is still described as a preview), since GitHub's
`releases/latest` route excludes releases marked as prereleases. The repository
and release downloads are public; verify the APK link without authentication.

1. Bump the npm, desktop Cargo manifest/lock, desktop help and Android manifest
   versions together. Increment Android's versionCode. Refresh the first-party
   desktop lock fingerprint in THIRD_PARTY_NOTICES.txt; dependency changes also
   require reviewing and regenerating the dependency license inventory.
2. Run launcher tests and push. Wait for the core, native and npm verification
   workflows for the exact source commit to succeed.
3. Dispatch the **Android APK** workflow on that commit's branch/ref. It uses
   `ANDROID_KEYSTORE_BASE64` and `ANDROID_KEYSTORE_PASSWORD` repository secrets
   with the `roomsync` alias. It fails if either is missing and never creates a
   replacement key. Retain a secure backup of this identity for future updates.
4. Download the `roomsync-android` and `roomsync-npm-preview` workflow artifacts.
   Check APK version, signature and SHA-256, and the npm package's version and
   bundled Windows executable integrity before publication.
5. Create a draft GitHub release at the exact verified commit, attach
   `RoomSync.apk`, its `SHA256SUMS.txt` and the npm tarball, then publish the release
   as Latest. Include any install-compatibility changes in the release notes.
6. Publish the verified npm tarball as `roomsync` with the `latest` tag. Verify the
   public registry version and tarball integrity after npm finishes processing it.
   A failed or pending authentication step is not a successful publication.

Version 0.1.7 introduces a new retained Android signing identity. It requires a
one-time uninstall of older preview installations; later releases should update
0.1.7 in place. Never replace this signing key silently.
