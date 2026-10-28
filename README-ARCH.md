# experimental/arch — architecture test bench for CDK + cdk-go bindings

**This branch is EXPERIMENTAL.** It exists to test CDK — and the
`cdk-ffi` static library the Go bindings (`cdk-go`) link against — on
the ROUTER architectures OpenWrt ships: `aarch64-unknown-linux-musl`,
`mipsel-unknown-linux-musl`, `mips-unknown-linux-musl`,
`arm-unknown-linux-musleabihf`, `x86_64-unknown-linux-musl`.

It is NOT for production, NOT a default build path, and NOT an
upstream proposal. Per Amperstrand's fork-first doctrine this is the
playground: patches land here, get audited and dogfooded on the bench,
and only a human may later gate an upstream submission.

## Why this branch exists

The official `cashubtc/cdk-go` releases ship prebuilt dynamic libraries
for desktop platforms only (linux amd64/arm64 glibc, macOS, Windows).
OpenWrt is musl on every architecture, so nothing published there can
link into a router binary. The library itself has no router-arch CI.
This branch carries whatever the router targets need — starting with
the known mipsel atomic-representation fix — plus the scripts that
prove the builds.

## What lives here

- `scripts/arch-build.sh <target>` — cross-builds the `cdk-ffi`
  staticlib for a Rust musl target using the host's cross C compilers
  (a staticlib never links libc; final linking happens in the Go build
  where the OpenWrt SDK's musl gcc applies).
- Target-specific fixes, each with a comment naming the arch and the
  failure it fixes. Nothing changes behavior on mainstream platforms.

## Upstream-target shape (for the eventual human-gated proposal)

The goal is that every fix here is either (a) already fixed upstream —
then this branch just tracks it — or (b) small and upstreamable, in
which case it graduates only after audit + dogfood, per doctrine.

## Status — measured 2026-10-28 (host cross-builds, cdk v0.18.2 base)

| Target | staticlib | Size | Objects | Fixes applied on this branch |
|---|---|---|---|---|
| aarch64-unknown-linux-musl | ✅ | 178 MB | 1297 ELF aarch64 | none needed |
| mipsel-unknown-linux-musl | ✅ | 157 MB | 912 ELF MIPS32r2 | (1)(2)(3) below |
| x86_64-unknown-linux-musl | not run yet | — | — | expected easy |

Everything compiles with DEFAULT cdk-ffi features (npubcash/nwc/bip353
— the full cdk-go surface).

### Fixes carried on this branch

1. **mipsel has no 64-bit atomics in std** (`AtomicU64` absent on
   32-bit MIPS). `nostr-relay-pool` 0.44.3 uses it; vendored at
   `third_party/nostr-relay-pool` with the import cfg'd to
   `portable-atomic` for 32-bit targets, wired via `[patch.crates-io]`.
   This is the upstreamable two-liner (per-target cfg import swap).
2. **rustc's staticlib bundling invokes a linker driver** over the
   crate's CGU objects; the default `cc` is the HOST compiler and dies
   on foreign ELF (`file in wrong format`). Fix: set the target linker
   to `rust-lld` (target-agnostic, ships with rustc) — and note cargo
   requires the env var NAME fully uppercased
   (`CARGO_TARGET_MIPSEL_UNKNOWN_LINUX_MUSL_LINKER`), unlike cc-rs's
   `CC_<target>` which accepts the lowercase form.
3. **cdylib crate-type dropped for cross builds**: linking a `.so`
   demands a target libc at build time, which cross builds don't have;
   the Go bindings consume the staticlib anyway.
4. **mipsel musl has no prebuilt std** on the toolchains we use —
   nightly `-Z build-std=std,panic_abort` compiles it from source
   (handled by arch-build.sh automatically).

Known follow-ups: strip/size-optimize the archives (unstripped ~160 MB
includes dep debug info; cdk-go's published desktop .a is ~133 MB, same
class); uniffi-bindgen is a HOST tool — never cross-build it
(`--lib` only); Go-side link config (staticlib + OpenWrt SDK musl gcc
at CGO time) is tbmg integration work, not this branch.

Prior art studied: jmcorgan/fips ships OpenWrt .ipk/.apk from Rust via
an SDK feed package with an ARCH→Rust-musl-target map and
`linker = $(TARGET_CC)` in a generated `.cargo/config.toml` — the
pattern this branch's builds align with.
