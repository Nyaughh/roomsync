#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
JAR="$ROOT/deps/zxing-core-3.5.4.jar"
EXPECTED=71de5d89341b5fcf5dd89da7f44e84d825d0e084cdf3ec77c9abe26b0f0ceb13
mkdir -p "$ROOT/deps"
if [[ ! -f "$JAR" ]]; then
  curl --fail --location --proto '=https' --tlsv1.2 --max-time 120 'https://repo.maven.apache.org/maven2/com/google/zxing/core/3.5.4/core-3.5.4.jar' -o "$JAR.download"
  echo "$EXPECTED  $JAR.download" | sha256sum --check --status
  mv "$JAR.download" "$JAR"
fi
echo "$EXPECTED  $JAR" | sha256sum --check --status
