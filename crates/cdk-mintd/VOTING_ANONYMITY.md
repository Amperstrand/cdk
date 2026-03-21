# Cashu Voting: Anonymity and Privacy

This document explains how Cashu voting preserves voter anonymity and how to configure a voting mint for maximum privacy.

## How Cashu Provides Anonymity

### Blind Signatures

Cashu uses **blind signatures** (Chaumian e-cash) to break the link between minting and spending:

1. **Minting**: User generates a random secret and blinds it, sending only the blinded value to the mint
2. **Signing**: The mint signs the blinded value without seeing the secret
3. **Unblinding**: User removes the blinding factor, revealing the mint's signature on their secret
4. **Spending**: User presents the secret + signature; the mint verifies without knowing who minted it

The mint cannot link a spent token back to the original minting transaction because the blinding factor is never revealed to the mint.

### Token Unlinkability

Each Cashu token is a bearer instrument containing:
- A random secret (never seen by mint during minting)
- A blind signature from the mint

When tokens are spent (melted for voting), the mint sees only:
- The secret (revealed at spend time)
- The signature (valid or invalid)

The mint cannot determine which minting session produced which token.

## Why Swaps May Not Be Necessary for Voting

In traditional e-cash systems, users often swap tokens to break linkability. However, for voting:

**Swaps are optional** because:

1. **Mint already saw your identity**: During minting, you identified yourself to get tokens (via Lightning payment or manual approval). The mint knows you received tokens.

2. **Blind signatures already protect**: When you melt tokens to vote, the mint cannot link those tokens back to your minting session due to blind signatures.

3. **Vote is just a melt**: Voting is a melt operation. You burn tokens to cast a vote. The mint sees "someone burned X sats for RED" but not "Alice burned X sats for RED."

### When Swaps DO Help

Swaps become useful if:
- You want to receive change in a different keyset (potentially different anonymity set)
- You're concerned about timing correlation (minting and voting at similar times)
- You want to split large amounts into smaller denominations

For most voting scenarios with manual approval, the blind signature already provides sufficient anonymity.

## Voting Flow with Anonymity

```
┌─────────────────────────────────────────────────────────────────┐
│ PHASE 1: TOKEN ACQUISITION (Identity Required)                  │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  Voter                      Mint                      Operator  │
│    │                         │                            │     │
│    │─── Request tokens ─────>│                            │     │
│    │                         │─── Manual approval ───────>│     │
│    │                         │<── Approved ───────────────│     │
│    │<── Blinded signature ───│                            │     │
│    │                         │                            │     │
│    │  [Voter unblinds locally]                            │     │
│    │  [Mint never saw the secret]                         │     │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│ PHASE 2: VOTING (Anonymous)                                     │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  Voter                      Mint                                │
│    │                         │                                  │
│    │─── Melt for RED ───────>│                                  │
│    │    (reveals secret)     │                                  │
│    │                         │─── Validates signature           │
│    │                         │─── Cannot link to minting        │
│    │                         │─── Records: "RED +50 sats"       │
│    │<── Vote confirmed ──────│                                  │
│    │                         │                                  │
│    │  [Mint knows vote + amount]                               │
│    │  [Mint does NOT know which voter]                         │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

## What the Mint Learns

| Information | Mint Knows? | Reason |
|-------------|-------------|--------|
| Who requested tokens | **Yes** | Manual approval / Lightning payment |
| How many tokens per person | **Yes** | Quote amounts are recorded |
| Which vote was cast | **Yes** | Vote option is in the melt request |
| Vote weight (sats burned) | **Yes** | Amount is in the melt request |
| *Which voter cast which vote* | **No** | Blind signatures break the link |
| *Correlation between mint and vote* | **Limited** | Only timing metadata |

## Privacy Recommendations

### For Voters

1. **Vote at different times**: Don't mint and immediately vote. Wait to reduce timing correlation.

2. **Consider swapping** (optional): If you want maximum privacy, swap tokens before voting:
   ```
   1. Mint tokens
   2. Swap tokens (creates new tokens with new secrets)
   3. Vote with swapped tokens
   ```
   This adds another unlinkability layer.

3. **Use consistent amounts**: If all votes are weighted differently, amount patterns might hint at identity. Use standard amounts if possible.

### For Mint Operators

1. **Set fees to zero for voting**:
   ```toml
   [fake_wallet]
   fee_percent = 0.0
   reserve_fee_min = 0
   voting_fee_sat = 0  # No voting fee
   ```

2. **Don't log timing**: Avoid correlating mint timestamps with vote timestamps in logs.

3. **Batch approvals**: Approve multiple quotes simultaneously to reduce timing correlation.

4. **Clear vote records after tally**: Vote tallies are in-memory. Restart the mint to clear them after the vote is complete.

## Configuration for Zero-Fee Voting

```toml
[fake_wallet]
supported_units = ["sat"]
fee_percent = 0.0           # No Lightning fee
reserve_fee_min = 0         # No minimum fee
min_delay_time = 0
max_delay_time = 1
manual_approval_incoming = true
voting_enabled = true
voting_options = ["RED", "BLUE"]
voting_topic = "Red vs Blue"
voting_fee_sat = 0          # No fee for voting
```

## Threat Model

### What This Protects Against

- **Mint operator**: Cannot determine how you voted (only that someone voted)
- **External observer**: Cannot see vote contents (HTTPS + Lightning encryption)
- **Token tracing**: Cannot follow tokens from mint to vote

### What This Does NOT Protect Against

- **Identity at minting**: You must identify to get tokens
- **Timing analysis**: Correlating mint time with vote time
- **Amount fingerprinting**: If you're the only one who minted 42 sats
- **Network-level tracking**: IP addresses, Lightning node IDs

## Comparison: Cashu Voting vs Alternatives

| Property | Cashu Voting | Lightning zaps | On-chain |
|----------|--------------|----------------|----------|
| Anonymous to mint | Partial (blind sigs) | No | No |
| Anonymous to public | Yes | Partial | No |
| Requires KYC | Configurable | No | No |
| Immediate finality | Yes | Yes | No (confirms) |
| Weighted voting | Yes (by amount) | Yes | Yes |
| No fees possible | Yes | Network fees | Miner fees |

## Summary

Cashu voting provides **practical anonymity** through blind signatures. The mint knows *who got tokens* and *what votes were cast* but cannot link the two. For most voting scenarios, this is sufficient without requiring swaps.

For maximum privacy-conscious voters:
1. Mint tokens
2. Wait (reduce timing correlation)
3. Optionally swap (adds unlinkability)
4. Vote

The result: The mint records "RED +50 sats" but cannot prove which voter cast it.

---

## Verifiability: Proving Your Vote Was Counted

**See [VOTING_VERIFIABILITY.md](./VOTING_VERIFIABILITY.md)** for how voters can verify their vote was included in the final tally using Merkle proofs.

**Key points:**
- DLEQ (NUT-12) proves token validity
- Merkle tree inclusion proofs prove tally inclusion
- Voters can verify: "My vote appears in the published list"
