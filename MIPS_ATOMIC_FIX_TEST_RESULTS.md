# MIPS AtomicUsize Fix — Test Results

## PR Under Test
- **Upstream PR**: https://github.com/cashubtc/cdk/pull/2258
- **Author**: thesimplekid (CDK core maintainer)
- **Title**: fix: use atomic usize for better target compatblity
- **State**: Open (not yet merged as of 2026-07-24)
- **Branch**: `thesimplekid:atomic_usize`

## Verification Environment
- **Host**: x86_64 Linux (Ubuntu 24.04)
- **Cross-target**: `mips-unknown-linux-gnu` (MIPS32, big-endian)
- **Cross-compiler**: `mips-linux-gnu-gcc` (gcc-12-mips-linux-gnu)
- **Rust**: nightly + `-Z build-std` (MIPS is Tier 3, no prebuilt std)

## Results

### cdk-common — Default Features

| Branch | Result | Time |
|--------|--------|------|
| `main` (without fix) | ✅ Compiles | ~47s |
| `atomic_usize` (with fix) | ✅ Compiles | ~21s (cached) |

### cdk-common — All Features (`--all-features`)

| Branch | Result | Error |
|--------|--------|-------|
| `main` (without fix) | ❌ Fails | `AtomicU64` not found in CDK test modules + `prometheus` |
| `atomic_usize` (with fix) | ❌ Fails on `prometheus` only | CDK's AtomicU64 is fixed; `prometheus` crate has its own AtomicU64 |

## Analysis

1. **CDK fix is correct**: `AtomicU64` → `AtomicUsize` in test modules resolves CDK's MIPS compilation issue
2. **Separate `prometheus` issue**: The `prometheus` crate (a dependency, not CDK code) also uses `AtomicU64`. This is a separate upstream issue.
3. **Default features work**: For production builds without `--all-features`, CDK compiles cleanly on MIPS

## Reproduction

```bash
# Install MIPS cross-compiler
sudo apt-get install -y gcc-mips-linux-gnu

# Install nightly Rust
rustup toolchain install nightly

# Clone the fix branch
git clone --depth=1 -b atomic_usize https://github.com/thesimplekid/cdk.git

# Create .cargo/config.toml
cat > cdk/.cargo/config.toml << 'CONF'
[unstable]
build-std = ["std", "panic_abort"]
[target.mips-unknown-linux-gnu]
linker = "mips-linux-gnu-gcc"
CONF

# Cross-compile cdk-common for MIPS
cd cdk
CC_mips_unknown_linux_gnu=mips-linux-gnu-gcc \
AR_mips_unknown_linux_gnu=mips-linux-gnu-ar \
cargo +nightly check -Z build-std --target mips-unknown-linux-gnu -p cdk-common
```

## Recommendation

The PR #2258 fix is correct, minimal, and necessary. It should be merged.
