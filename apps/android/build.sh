#!/usr/bin/env bash
# Minimal deterministic SDK build: no Gradle daemon, Maven, AndroidX, telemetry or
# downloaded build plugins. Install official SDK components before invoking.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
: "${SDK:?Set ANDROID_HOME to the installed Android SDK directory}"
NDK="${ANDROID_NDK_HOME:-$SDK/ndk/27.2.12479018}"
BT="${ANDROID_BUILD_TOOLS:-$SDK/build-tools/35.0.0}"
PLATFORM="$SDK/platforms/android-35/android.jar"
LLVM="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"
for file in "$PLATFORM" "$BT/aapt2" "$BT/d8" "$BT/zipalign" "$BT/apksigner" "$LLVM/aarch64-linux-android26-clang"; do
  [[ -f "$file" ]] || { echo "Missing SDK component: $file" >&2; exit 1; }
done
bash "$ROOT/scripts/fetch-deps.sh"
QR="$ROOT/deps/zxing-core-3.5.4.jar"
BUILD="$ROOT/build"
mkdir -p "$BUILD" "$BUILD/classes" "$BUILD/generated" "$BUILD/dex" "$BUILD/apk/lib/arm64-v8a" "$BUILD/apk/lib/x86_64"
NOTICES="$ROOT/../../docs/THIRD_PARTY_NOTICES.txt"
[[ -s "$NOTICES" ]] || { echo "Missing native dependency notices: $NOTICES" >&2; exit 1; }
rm -rf "$BUILD/assets"
mkdir -p "$BUILD/assets"
cp "$ROOT/app/src/main/assets/"*.txt "$BUILD/assets/"
cp "$NOTICES" "$BUILD/assets/THIRD_PARTY_NOTICES.txt"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$LLVM/aarch64-linux-android26-clang"
export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$LLVM/x86_64-linux-android26-clang"
export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,-z,max-page-size=16384"
cargo build --locked --manifest-path "$ROOT/native/Cargo.toml" --release --target aarch64-linux-android --target x86_64-linux-android
cp "$ROOT/native/target/aarch64-linux-android/release/libroomsync_android.so" "$BUILD/apk/lib/arm64-v8a/"
cp "$ROOT/native/target/x86_64-linux-android/release/libroomsync_android.so" "$BUILD/apk/lib/x86_64/"
rm -rf "$BUILD/classes" "$BUILD/generated" "$BUILD/dex"
mkdir -p "$BUILD/classes" "$BUILD/generated" "$BUILD/dex"
"$BT/aapt2" compile --dir "$ROOT/app/src/main/res" -o "$BUILD/resources.zip"
"$BT/aapt2" link -o "$BUILD/base.apk" -I "$PLATFORM" --manifest "$ROOT/app/src/main/AndroidManifest.xml" -A "$BUILD/assets" --java "$BUILD/generated" --min-sdk-version 26 --target-sdk-version 35 "$BUILD/resources.zip"
find "$ROOT/app/src/main/java" "$BUILD/generated" -name '*.java' | sort > "$BUILD/sources.txt"
# Some cloud JDK images include javac as a module without its shell launcher.
java -m jdk.compiler/com.sun.tools.javac.Main -source 8 -target 8 -classpath "$PLATFORM:$QR" -Xlint:all,-options -d "$BUILD/classes" @"$BUILD/sources.txt"
find "$BUILD/classes" -name '*.class' | sort > "$BUILD/classes.txt"
"$BT/d8" --lib "$PLATFORM" --min-api 26 --output "$BUILD/dex" @"$BUILD/classes.txt" "$QR"
python3 - "$BUILD" <<'PY'
import pathlib,shutil,sys,zipfile
b=pathlib.Path(sys.argv[1]);shutil.copyfile(b/'base.apk',b/'unsigned.apk')
with zipfile.ZipFile(b/'unsigned.apk','a') as z:
    for p in sorted((b/'dex').glob('*.dex')): z.write(p,p.name,compress_type=zipfile.ZIP_STORED)
    for p in sorted((b/'apk').glob('lib/*/*.so')): z.write(p,p.relative_to(b/'apk'),compress_type=zipfile.ZIP_STORED)
PY
"$BT/zipalign" -P 16 -f 4 "$BUILD/unsigned.apk" "$BUILD/aligned.apk"
if [[ "${ROOMSYNC_UNSIGNED_ONLY:-0}" == 1 ]]; then
    cp "$BUILD/aligned.apk" "$BUILD/RoomSync-unsigned.apk"
    "$BT/zipalign" -c -P 16 4 "$BUILD/RoomSync-unsigned.apk"
    sha256sum "$BUILD/RoomSync-unsigned.apk"
    echo "Built unsigned verification APK. It cannot be installed until signed."
    exit 0
fi
KEY="${ROOMSYNC_DEBUG_KEYSTORE:-$BUILD/debug.keystore}"
if [[ ! -f "$KEY" ]]; then
    [[ "${ROOMSYNC_ALLOW_NEW_DEBUG_KEY:-0}" == 1 ]] || { echo "Signing key missing. Restore the original keystore for an in-place update. Use ROOMSYNC_UNSIGNED_ONLY=1 to verify compilation. A new key cannot update an existing install." >&2; exit 1; }
    echo "Creating a NEW debug signing identity. This APK cannot update an app signed by the lost key." >&2
    keytool -genkeypair -keystore "$KEY" -alias androiddebugkey -storepass android -keypass android -keyalg RSA -keysize 2048 -validity 3650 -dname 'CN=RoomSync Debug,O=RoomSync,C=US' >/dev/null
    chmod 600 "$KEY"
fi
"$BT/apksigner" sign --ks "$KEY" --ks-key-alias androiddebugkey --ks-pass pass:android --key-pass pass:android --out "$BUILD/RoomSync-debug.apk" "$BUILD/aligned.apk"
"$BT/apksigner" verify --verbose "$BUILD/RoomSync-debug.apk"
"$BT/zipalign" -c -P 16 4 "$BUILD/RoomSync-debug.apk"
sha256sum "$BUILD/RoomSync-debug.apk"
echo "Built: $BUILD/RoomSync-debug.apk (arm64-v8a + x86_64; Android 8.0+)"
