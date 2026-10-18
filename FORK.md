# Amperstrand/cdk — the carried delta

This fork tracks [cashubtc/cdk](https://github.com/cashubtc/cdk) upstream and
carries ONE delta: **OpenWrt packaging and CI for `cdk-mintd-wrt`**
(the TollGate bench-mint / router-mint lane). Everything else follows
upstream; re-syncs are rebase-carries of this file set.

## What is carried (see `rebase/cdk-upstream-sync` PRs for history)

- `.github/workflows/build-openwrt-package.yml` — builds `cdk-mintd` for
  OpenWrt: `x86_64-unknown-linux-musl` on `ubuntu-latest` (musl-tools +
  musl.cc), `aarch64-unknown-linux-musl` on the **native**
  `ubuntu-24.04-arm` runner (no cross-compiler for arm64), packaged as
  `.ipk` by `packaging/build-ipk.sh`. MIPS is deliberately NOT in the
  matrix — see `MIPS_ATOMIC_FIX_TEST_RESULTS.md` (upstream PR #2258,
  still open).
- `packaging/` — `build-ipk.sh`, `/etc/init.d/cdk-mintd-wrt`, `postinst`,
  firewall fragment, and the default `mintd.toml` (fakewallet + sqlite;
  fields re-validated against `crates/cdk-mintd/src/config.rs` at each
  re-sync).

## What is NOT carried (deliberately dropped at the 2026-10 re-sync)

- The `test`-feature removals in `cdk-sqlite`/`cdk-sql-common` — upstream
  kept the feature and added `paste` for the `*_db_test!` macro sites.
- `Cross.toml` and `packaging/cross/` Dockerfiles — dead endpoints of the
  cross-rs iterations, replaced by the native-runner shape above.

## Drift policy

Upstream drift is re-based (not merged) at each sync; the carried set is
the endpoint, not the history. **Drift-report threshold for this fork:
5 commits** (was effectively "whenever noticed" — the 2026-10 sync found
614). Local commits beyond 5 without an upstream sync = the report fires.
