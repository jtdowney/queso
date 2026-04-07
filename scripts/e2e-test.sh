#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "==> Building queso..."
cargo build --release --manifest-path "$PROJECT_ROOT/Cargo.toml"
QUESO="$PROJECT_ROOT/target/release/queso"

WORK_DIR=$(mktemp -d)
trap 'rm -rf "$WORK_DIR"' EXIT

echo "==> Creating test Gleam project..."
cd "$WORK_DIR"
gleam new test_app

echo "==> Building with queso..."
cd test_app
"$QUESO" build

echo "==> Finding output binary..."
BINARY=""
for f in build/queso/test_app-*; do
  if [ -f "$f" ]; then
    BINARY="$f"
    break
  fi
done

if [ -z "$BINARY" ]; then
  echo "ERROR: No output binary found in build/queso/"
  ls -la build/queso/ 2>/dev/null || true
  exit 1
fi

echo "==> Running $BINARY..."
OUTPUT=$("./$BINARY")

if echo "$OUTPUT" | grep -q "Hello from test_app!"; then
  echo "PASS"
else
  echo "FAIL: Expected 'Hello from test_app!' in output:"
  echo "$OUTPUT"
  exit 1
fi
