# fix/duplicate-outputs-error-identity — measured evidence (2026-10-29)

Rig: host cdk-mintd (this branch) + two cdk-walletd (0.18.2, unpatched
wallet) sharing one seed against a fakewallet mint. Live chain:

1. W1 mints 1000 sat → keyset counter [0..54) exposed to the mint.
2. W2 (same seed, fresh store) mints 500 → 11003 "Blinded Message is
   already signed" (identity fix visible on the wire; unpatched mint
   returned "Request already paid").
3. W2 fresh quote, same 500 → 11003 again. keyset_counter = 54 after
   the two attempts (27 secrets each) — the wallet's saga compensation
   deliberately keeps counter increments, so each refusal walks the
   derivation window forward.
4. W2 third fresh quote → SUCCESS, 500 minted, counter 81.

Verdict:
- With the correct NUT-19 11003 identity the wallet self-heals by
  walking past the burned range in ceil(burn_depth/secrets_per_mint)
  attempts. Unpatched, the InvoiceAlreadyPaid quote-failure path rolls
  the counter back and the wallet collides forever (permanent brick,
  reproduced 2026-10-28).
- The wallet has no skip-ahead handler for 11003 (optional follow-up:
  on collision, advance the counter past the mint's known-signed range
  in one step, restore-style). Not required for correctness.

Parity pin: tests/cloud-lab/parity test_40 in
OpenTollGate/tollgate-module-basic-go (cdk-migration branch) pins the
UNPATCHED brick; flip it when this mint ships.
