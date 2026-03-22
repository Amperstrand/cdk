# Cashu Keyset Voting PoC

This document describes a keyset-based voting exploration where votes are trapped e-cash tokens, not Lightning payments.

## Goal

- Use dedicated vote keysets for each issue/option.
- Keep vote tokens non-monetary and non-redeemable by policy.
- Preserve unlinkability between issuance and final cast as much as possible for a fast PoC.
- Run a deploy-targeted CI demo to validate setup and assumptions.

## Model

For an issue like `election2026` with options `RED` and `BLUE`, mintd provisions:

- `vote_election2026_red`
- `vote_election2026_blue`

These are custom `CurrencyUnit` values and independent keysets.

Vote tokens are intended to be:

1. Issued to voters through normal mint authorization flow
2. Used for voting logic
3. Not redeemable back to monetary units

## Important Constraint in Current CDK

Current swap verification enforces same-unit input/output transactions. That means direct `sat -> vote_unit` swaps are not available without explicit mint policy changes.

This branch is therefore an exploration branch focused on:

- keyset provisioning strategy
- threat-model and policy documentation
- deployment CI demo checks

and not a complete production voting protocol.

## Configuration

Add vote keyset definitions under `[fake_wallet]`:

```toml
[fake_wallet]
supported_units = ["sat"]
manual_approval_incoming = true

vote_keysets = [
  { issue = "election2026", options = ["RED", "BLUE"] },
  { issue = "budget2026", options = ["YES", "NO"] }
]
```

On startup, mintd will ensure active keysets exist for each `(issue, option)` pair.

## Environment Variable Override

You can set vote keysets through:

- `CDK_MINTD_FAKE_WALLET_VOTE_KEYSETS`

Format:

```text
issue1:OPTION_A|OPTION_B;issue2:YES|NO
```

Example:

```text
CDK_MINTD_FAKE_WALLET_VOTE_KEYSETS="election2026:RED|BLUE;budget2026:YES|NO"
```

## Threat Model (PoC)

Protected in this branch:

- auditable vote-ledger publication and verification APIs
- keyset separation per issue/option
- deploy-time detection of keyset provisioning

Not solved in this branch:

- coercion resistance
- receipt-freeness guarantees
- threshold trust minimization
- protocol-level non-redeemability

## CI Demo

Workflow: `.github/workflows/voting-keyset-live-e2e.yml`

Script: `crates/cdk-mintd/scripts/ci_keyset_voting_e2e.py`

Checks:

1. Mint is reachable
2. Expected vote keyset units exist and are active
3. Vote endpoints are reachable
4. Outputs a deployment summary for human review

## Next Iteration

1. Add explicit mint policy guards to block melt/redeem for `vote_*` units
2. Add vote-cast endpoint that consumes vote proofs directly
3. Add double-vote prevention and deterministic tally lifecycle
4. Add optional anti-coercion mode variants in docs and test matrix
