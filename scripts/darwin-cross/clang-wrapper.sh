#!/bin/sh
# cc-rs wrapper: routes Apple-clang style arguments to `zig cc` so C
# dependencies (e.g. ring, psm) can cross-compile to Darwin targets from a
# Linux host. zig bundles the Darwin libc headers and produces real Mach-O
# objects. `scripts/check-darwin.sh` replaces __ZIG_TARGET__ with the zig
# triple for the chosen cargo target.
#
# cc-rs injects clang triples like `--target=arm64-apple-macosx` /
# `arm64-apple-macos`; zig's grammar wants `aarch64-macos` (no `arm64`
# alias, no `apple` vendor, no `macosx` OS name). `-gfull` is
# Darwin-clang-only and is dropped.
args=""
for a in "$@"; do
  case "$a" in
    *arm64-apple-macos*|*aarch64-apple-macos*|*x86_64-apple-macos*)
      a="${a/arm64-apple-macos/__ZIG_TARGET__}"
      a="${a/aarch64-apple-macosx/__ZIG_TARGET__}"
      a="${a/aarch64-macosx/__ZIG_TARGET__}"
      a="${a/x86_64-apple-macosx/__ZIG_TARGET__}"
      a="${a/x86_64-macosx/__ZIG_TARGET__}"
      ;;
    "-gfull") continue;;
  esac
  args="$args \"$a\""
done
eval exec zig cc "$args"
