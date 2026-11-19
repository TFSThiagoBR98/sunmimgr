#!/bin/sh
# Cross-compilation check for aarch64-apple-darwin (macOS Apple Silicon)
# from a Linux host using zig as the Darwin C cross-compiler.
#
# Scope:
#   - sunmi-core + sunmi-adb: full `cargo check` (validated on Linux).
#   - sunmi-ui (gpui): the macOS backend pulls `gpui_media`, whose build
#     script needs bindgen against the macOS SDK (`xcrun --show-sdk-path`),
#     so a full cross-check requires an SDK checkout (osxcross-style) — on
#     a real Mac or a CI macos-14 runner everything builds natively.
#
# Usage: ./scripts/check-darwin.sh [target]
set -e

TARGET="${1:-aarch64-apple-darwin}"
DIR="$(cd "$(dirname "$0")" && pwd)"

if ! command -v zig >/dev/null 2>&1; then
  echo "error: zig is required on PATH (https://ziglang.org)" >&2
  exit 1
fi

case "$TARGET" in
  aarch64-apple-darwin) ZIG_TARGET="aarch64-macos" ;;
  x86_64-apple-darwin)  ZIG_TARGET="x86_64-macos" ;;
  *) echo "unsupported target: $TARGET" >&2; exit 1 ;;
esac

# Rewrite the zig target inside the wrapper (placeholder swap).
sed "s/__ZIG_TARGET__/$ZIG_TARGET/g" \
  "$DIR/darwin-cross/clang-wrapper.sh" > "$DIR/clang-wrapper-$TARGET"
chmod +x "$DIR/clang-wrapper-$TARGET"
export CC_${TARGET//-/_}="$DIR/clang-wrapper-$TARGET"
export AR_${TARGET//-/_}="zig ar"

shift_target="$TARGET"
shift 0
echo "== cargo check -p sunmi-core -p sunmi-adb --target $shift_target =="
cargo check -p sunmi-core -p sunmi-adb --target "$shift_target"

echo ""
echo "== cargo check --workspace --target $shift_target (requires macOS SDK; expected to stop at gpui_media on a Linux host) =="
cargo check --workspace --target "$shift_target" || echo "note: full workspace check needs a macOS SDK or a native runner (see .github/workflows/darwin-check.yml)"
