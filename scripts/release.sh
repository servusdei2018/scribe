#!/usr/bin/env bash
# Cross-platform release packaging for scribe.
#
# Produces statically-linked / platform-native archives for:
#   * Linux x86_64  (x86_64-unknown-linux-musl, fully static)
#   * Linux aarch64 (aarch64-unknown-linux-musl, fully static)
#   * macOS universal (x86_64-apple-darwin + aarch64-apple-darwin via lipo)
#   * Windows x86_64 (x86_64-pc-windows-msvc)
#
# This script is configuration only: it is NOT run during the phase-30 gates
# because the musl/aarch64/macOS toolchains are not installed on the build
# host. Install the targets (and `cross` for musl/aarch64) before invoking.
#
# Usage:
#   ./scripts/release.sh --check     # report which toolchains are available
#   ./scripts/release.sh             # build every target that is available
#   RELEASE_TARGETS="x86_64-unknown-linux-musl" ./scripts/release.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="$ROOT/dist"
PKG="scribe-cli"
BINARY="scribe"
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')"

ALL_TARGETS=(
  "x86_64-unknown-linux-musl"
  "aarch64-unknown-linux-musl"
  "x86_64-apple-darwin"
  "aarch64-apple-darwin"
  "x86_64-pc-windows-msvc"
)

if [[ "${1:-}" == "--check" ]]; then
  echo "scribe $VERSION release toolchain check"
  echo "  rustc:        $(rustc --version 2>/dev/null || echo MISSING)"
  echo "  cargo:        $(cargo --version 2>/dev/null || echo MISSING)"
  echo "  cross:        $(command -v cross >/dev/null && cross --version | head -1 || echo MISSING)"
  echo "  lipo:         $(command -v lipo >/dev/null && echo available || echo MISSING)"
  echo "  installed targets:"
  rustup target list --installed 2>/dev/null | sed 's/^/    /' || echo "    (rustup not available)"
  exit 0
fi

mkdir -p "$DIST"
cd "$ROOT"

use_cross() {
  command -v cross >/dev/null 2>&1 && [[ "$1" == *musl* || "$1" == *aarch64* ]]
}

build_target() {
  local target="$1"
  local out="$DIST/$target"
  mkdir -p "$out"
  echo "==> building $target"
  if use_cross "$target"; then
    cross build --release -p "$PKG" --target "$target"
  else
    cargo build --release -p "$PKG" --target "$target"
  fi
  local bin="$ROOT/target/$target/release/$BINARY"
  if [[ "$target" == *windows* ]]; then
    bin="$bin.exe"
  fi
  if [[ ! -f "$bin" ]]; then
    echo "  !! binary not produced at $bin (target toolchain may be missing); skipping"
    return 0
  fi
  if [[ "$target" == *windows* ]]; then
    (cd "$out" && zip -q "$DIST/${BINARY}-${VERSION}-${target}.zip" "$BINARY.exe")
  else
    cp "$bin" "$out/$BINARY"
    (cd "$DIST" && tar czf "${BINARY}-${VERSION}-${target}.tar.gz" "$target")
  fi
  echo "  packaged $target"
}

if [[ -n "${RELEASE_TARGETS:-}" ]]; then
  read -r -a TARGETS <<<"$RELEASE_TARGETS"
else
  TARGETS=("${ALL_TARGETS[@]}")
fi

for target in "${TARGETS[@]}"; do
  build_target "$target"
done

# macOS universal binary: fatten the two Darwin slices when both are present.
if command -v lipo >/dev/null 2>&1 \
   && [[ -f "$ROOT/target/x86_64-apple-darwin/release/$BINARY" ]] \
   && [[ -f "$ROOT/target/aarch64-apple-darwin/release/$BINARY" ]]; then
  echo "==> creating macOS universal binary"
  mkdir -p "$DIST/macos-universal"
  lipo -create \
    "$ROOT/target/x86_64-apple-darwin/release/$BINARY" \
    "$ROOT/target/aarch64-apple-darwin/release/$BINARY" \
    -output "$DIST/macos-universal/$BINARY"
  (cd "$DIST" && tar czf "${BINARY}-${VERSION}-macos-universal.tar.gz" macos-universal)
fi

echo "release artifacts in $DIST:"
ls -lh "$DIST"/*.tar.gz "$DIST"/*.zip 2>/dev/null || echo "  (none)"
