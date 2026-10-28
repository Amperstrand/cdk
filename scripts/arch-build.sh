#!/usr/bin/env bash
# arch-build.sh — cross-build the cdk-ffi static library for a router
# (musl) target, on a host with the matching *-linux-gnu cross gcc.
#
# A staticlib never links libc: the cross gcc only compiles C objects
# (secp256k1, ring, …) for the target. The FINAL link happens in the Go
# build (CGO), where the OpenWrt SDK's musl gcc supplies the libc.
#
# Targets without prebuilt std on the pinned toolchain (mipsel/mips
# musl) are built with nightly `-Z build-std` — the path the tollgate
# wallet research measured for mipsel.
#
# Usage: scripts/arch-build.sh <rust-musl-target>
#   e.g. scripts/arch-build.sh aarch64-unknown-linux-musl
set -euo pipefail
cd "$(dirname "$0")/.."

TARGET="${1:?usage: arch-build.sh <rust-musl-target>}"
# cargo's target dir can come from env OR ~/.cargo/config.toml — ask cargo.
OUT_DIR="$(cargo metadata --format-version 1 --no-deps 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')/$TARGET/release"
PINNED="$(grep -oP 'channel = "\K[^"]+' rust-toolchain.toml)"

case "$TARGET" in
  aarch64-unknown-linux-musl) CROSS=aarch64-linux-gnu ;;
  mipsel-unknown-linux-musl)  CROSS=mipsel-linux-gnu ;;
  mips-unknown-linux-musl)    CROSS=mips-linux-gnu ;;
  arm-unknown-linux-musleabihf) CROSS=arm-linux-gnueabihf ;;
  x86_64-unknown-linux-musl)  CROSS=x86_64-linux-musl ;;
  *) echo "no cross gcc mapping for $TARGET" >&2; exit 2 ;;
esac

TUS="${TARGET//-/_}"
export CC_"$TUS"="$CROSS-gcc"
export AR_"$TUS"="$CROSS-ar"
# rustc's staticlib bundling runs a linker driver over the crate's CGU
# objects; the default `cc` is the HOST compiler and dies on foreign
# ELF. rust-lld is target-agnostic and ships with rustc. NOTE: cargo
# requires the env NAME fully uppercased (CC_/AR_ below are read by
# cc-rs, which accepts the lowercase target form).
export CARGO_TARGET_"$(echo "$TUS" | tr 'a-z' 'A-Z')"_LINKER="rust-lld"

CARGO=(cargo)
EXTRA=()
if rustup target list --toolchain "$PINNED" --installed 2>/dev/null | grep -qx "$TARGET" \
   || rustup target add --toolchain "$PINNED" "$TARGET" >/dev/null 2>&1; then
  : # prebuilt std available on the pinned toolchain
else
  echo "== $TARGET: no prebuilt std on toolchain $PINNED — nightly -Z build-std =="
  CARGO=(cargo +nightly)
  EXTRA=(-Z build-std=std,panic_abort)
  rustup component add rust-src --toolchain nightly >/dev/null 2>&1 || true
fi

echo "== cdk-ffi staticlib for $TARGET (cc=$CROSS-gcc) =="
# Default features (npubcash/nwc/bip353) = the full cdk-go surface.
# --lib only: uniffi-bindgen is a HOST tool and must not be cross-built
# (cross-building it links target CRT objects into the host ld).
"${CARGO[@]}" build --release -p cdk-ffi --lib --target "$TARGET" "${EXTRA[@]}"
echo "artifact: $OUT_DIR/libcdk_ffi.a"
ls -la "$OUT_DIR/libcdk_ffi.a"
