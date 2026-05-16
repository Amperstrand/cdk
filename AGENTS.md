# AGENTS.md — Amperstrand CDK Fork

This is the **Amperstrand experimental fork** of [cashubtc/cdk](https://github.com/cashubtc/cdk).

## CRITICAL: Upstream Boundary

**This repository is a private fork of a public open-source project. The upstream project is maintained by other people.**

### ABSOLUTE RULES

1. **NEVER push to `cashubtc/cdk` (the upstream).** No direct commits, no force pushes, no branch creation, no issue comments, no PRs, nothing. The upstream remote exists for pulling upstream changes only.

2. **NEVER open issues or PRs on the upstream repo.** If upstream contributions are desired, they will be carefully prepared and submitted by the project owner through a deliberate, reviewed process — not by an automated agent.

3. **NEVER interact with the upstream repo's GitHub in any way** — no comments, no reactions, no issue creation, no discussions, no wiki edits. Zero. The upstream maintainers should never see activity from this fork.

4. **All work happens on the `origin` remote** (`git@github.com:Amperstrand/cdk.git`). Branch from `experimentalaislop`, push to `origin`.

5. **When syncing from upstream**, always add an `upstream` remote pointing to `https://github.com/cashubtc/cdk.git`, fetch from it, and merge/rebase into local branches. Never push those merged changes back to `upstream`.

### Git Remote Configuration

```
origin    → git@github.com:Amperstrand/cdk.git                    (READ-WRITE, this fork)
upstream  → https://github.com/cashubtc/cdk.git                   (READ-ONLY upstream, fetch only — add manually)
```

- `git fetch upstream main && git merge upstream/main` — OK (sync from upstream)
- `git push origin experimentalaislop` — OK (work on our fork)
- `git push upstream anything` — **FORBIDDEN**
- `gh issue create --repo cashubtc/cdk` — **FORBIDDEN**
- `gh pr create --repo cashubtc/cdk` — **FORBIDDEN**

## Default Branch

`experimentalaislop` — all development happens here. Do not use `main` or `master`.

## Project Overview

This fork exists for **experimental AI research and building OpenWrt packages** of `cdk-mintd`.

The primary goal is to produce a lightweight `cdk-mintd` OpenWrt package (`.ipk`) that runs on routers alongside [tollgate-rs](https://github.com/Amperstrand/tollgate-rs-ai-research-and-experiments). The mint provides the Cashu ecash backend that tollgate-rs uses for token payments.

### What We Build

- **`cdk-mintd-wrt`** — OpenWrt package containing a stripped-down `cdk-mintd` binary compiled with `--no-default-features --features "sqlite fakewallet"`. This drops CLN, LND, LNBits, management-rpc, grpc-processor, payment-processor, and signatory — leaving only SQLite storage and the fake wallet (perfect for testing and development).

### Compilation Flags

```bash
cross build --release --target <triple> \
  --bin cdk-mintd \
  --no-default-features \
  --features "sqlite fakewallet"
```

This produces a minimal binary suitable for resource-constrained routers.

### Known Cross-Compilation Issues

1. **`rusqlite` bundled feature**: `cdk-sqlite` uses `rusqlite` with the `bundled` feature, which compiles SQLite from C source. This works for cross-compilation via `cross` but requires `cc` and potentially `cmake` in the cross container. The default `cross` Docker images include these tools.

2. **`cdk-common` test feature bug**: There is a known bug where `cdk-common` enables a "test" feature unconditionally that breaks MIPS cross-compilation. Our fork has the `fix/v0.16-remove-test-feature` branch that patches this. The `experimentalaislop` branch should include or be based on this fix.

## OpenWrt Package Structure

```
packaging/
├── build-ipk.sh                              # .ipk assembly script (ar + tar)
├── postinst                                  # Post-install script
└── files/
    └── etc/
        ├── init.d/cdk-mintd-wrt              # procd init script
        ├── config/firewall-cdk-mintd         # Firewall rules (port 8085)
        └── tollgate/mintd.toml               # Default config (fakewallet, sqlite, 0.0.0.0:8085)
```

### Package Details

| Field | Value |
|-------|-------|
| Package name | `cdk-mintd-wrt` |
| Binary | `/usr/bin/cdk-mintd-wrt` |
| Config | `/etc/tollgate/mintd.toml` |
| Database dir | `/tmp/cdk-mintd/` (RAM, volatile) |
| Listen | `0.0.0.0:8085` |
| Mint URL | `http://tollgate.lan:8085/` |
| LN backend | `fakewallet` (auto-pays all invoices) |

## CI Workflow

`.github/workflows/build-openwrt-package.yml` triggers on push to `experimentalaislop` only.

Builds two targets:
- **x86_64** (`x86_64-unknown-linux-musl`) — for initial testing
- **arm64** (`aarch64-unknown-linux-musl`) — for ARM routers

Produces `.ipk` artifacts downloadable from the GitHub Actions run.

## Integration with tollgate-rs

tollgate-rs references this mint as `http://tollgate.lan:8085/` in its `accepted_mints` configuration. The two packages (`tollgate-wrt` and `cdk-mintd-wrt`) are designed to run on the same router.

## Working Conventions

### Branching

- Branch from `experimentalaislop` on the `origin` remote
- Branch naming: descriptive (e.g., `openwrt-packaging`, `fix/sqlite-cross-compile`)
- Push branches to `origin` only

### What NOT to Touch

- **NEVER modify existing CDK crate source code** — no changes to `crates/*/src/`
- **NEVER touch Cargo.toml files in crates/** — feature flags and dependencies stay as upstream intended
- **NEVER modify the existing CDK CI workflows** (`.github/workflows/ci.yml`, etc.)
- **NEVER add dependencies** to any crate

### What We CAN Touch

- `packaging/` — our packaging scripts and configs
- `.github/workflows/build-openwrt-package.yml` — our CI workflow (new file)
- `AGENTS.md` — this file
- Branch-level changes that don't modify crate source

## Related Projects

- [tollgate-rs](https://github.com/Amperstrand/tollgate-rs-ai-research-and-experiments) — Rust TollGate implementation, consumes this mint
- [cashubtc/cdk](https://github.com/cashubtc/cdk) — Upstream CDK (READ-ONLY reference)
