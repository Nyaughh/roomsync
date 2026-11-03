#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
cargo fmt --manifest-path "$ROOT/native/Cargo.toml" --check
cargo test --locked --manifest-path "$ROOT/native/Cargo.toml"
cargo clippy --locked --manifest-path "$ROOT/native/Cargo.toml" --all-targets -- -D warnings
mkdir -p "$ROOT/build/test-classes"
java -m jdk.compiler/com.sun.tools.javac.Main -d "$ROOT/build/test-classes" "$ROOT/app/src/main/java/dev/roomsync/Timing.java" "$ROOT/tests/TimingTest.java" "$ROOT/app/src/main/java/dev/roomsync/OutputBudget.java" "$ROOT/tests/OutputBudgetTest.java" "$ROOT/app/src/main/java/dev/roomsync/FocusState.java" "$ROOT/tests/FocusStateTest.java"
java -cp "$ROOT/build/test-classes" dev.roomsync.TimingTest
java -cp "$ROOT/build/test-classes" dev.roomsync.OutputBudgetTest
java -cp "$ROOT/build/test-classes" dev.roomsync.FocusStateTest
cargo build --locked --manifest-path "$ROOT/native/Cargo.toml"
java -m jdk.compiler/com.sun.tools.javac.Main -d "$ROOT/build/test-classes" "$ROOT/app/src/main/java/dev/roomsync/NativeReceiver.java" "$ROOT/tests/NativeSmoke.java"
java -Djava.library.path="$ROOT/native/target/debug" -cp "$ROOT/build/test-classes" dev.roomsync.NativeSmoke

java -m jdk.compiler/com.sun.tools.javac.Main -d "$ROOT/build/test-classes" "$ROOT/app/src/main/java/dev/roomsync/PairingConfig.java" "$ROOT/app/src/main/java/dev/roomsync/PairingMemory.java" "$ROOT/tests/PairingTest.java"
java -cp "$ROOT/build/test-classes" dev.roomsync.PairingTest
bash "$ROOT/scripts/fetch-deps.sh"
QR="$ROOT/deps/zxing-core-3.5.4.jar"
java -m jdk.compiler/com.sun.tools.javac.Main -classpath "$QR:$ROOT/build/test-classes" -d "$ROOT/build/test-classes" "$ROOT/tests/QrTest.java"
java -cp "$QR:$ROOT/build/test-classes" dev.roomsync.QrTest

cargo fmt --manifest-path "$ROOT/tests/live_fixture/Cargo.toml" --check
cargo check --locked --manifest-path "$ROOT/tests/live_fixture/Cargo.toml"
java -m jdk.compiler/com.sun.tools.javac.Main -classpath "$ROOT/build/test-classes" -d "$ROOT/build/test-classes" "$ROOT/tests/LiveTimingTest.java"
if [[ "${ROOMSYNC_LIVE_TIMING:-0}" == 1 ]]; then
    cargo build --release --locked --manifest-path "$ROOT/tests/live_fixture/Cargo.toml"
    cargo build --release --locked --manifest-path "$ROOT/native/Cargo.toml"
    java -Djava.library.path="$ROOT/native/target/release" -cp "$ROOT/build/test-classes" dev.roomsync.LiveTimingTest "$ROOT/tests/live_fixture/target/release/roomsync-live-fixture"
else
    echo "Optional wall-clock JNI/UDP timing smoke: set ROOMSYNC_LIVE_TIMING=1. Deterministic live-source correctness is tested by apps/desktop cargo tests."
fi
