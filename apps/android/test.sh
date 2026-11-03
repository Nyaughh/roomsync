#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
cargo fmt --manifest-path "$ROOT/native/Cargo.toml" --check
cargo test --locked --manifest-path "$ROOT/native/Cargo.toml"
cargo clippy --locked --manifest-path "$ROOT/native/Cargo.toml" --all-targets -- -D warnings
mkdir -p "$ROOT/build/test-classes"
java -m jdk.compiler/com.sun.tools.javac.Main -d "$ROOT/build/test-classes" "$ROOT/app/src/main/java/dev/roomsync/Timing.java" "$ROOT/tests/TimingTest.java"
java -cp "$ROOT/build/test-classes" dev.roomsync.TimingTest
cargo build --locked --manifest-path "$ROOT/native/Cargo.toml"
java -m jdk.compiler/com.sun.tools.javac.Main -d "$ROOT/build/test-classes" "$ROOT/app/src/main/java/dev/roomsync/NativeReceiver.java" "$ROOT/tests/NativeSmoke.java"
java -Djava.library.path="$ROOT/native/target/debug" -cp "$ROOT/build/test-classes" dev.roomsync.NativeSmoke
