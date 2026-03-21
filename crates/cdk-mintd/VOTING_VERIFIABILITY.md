# DLEQ Proofs for Vote Verification

## What DLEQ Proofs Are (NUT-12)

DLEQ proofs are **Discrete Log Equality** proofs — they prove that two values share the same discrete logarithm without revealing it.

### In Cashu Context

When the mint signs your blinded message `B'` with key `k`,- **Signature**: `C' = k × B'`
- **DLEQ proof**: `{e, s}` proving `C'` was computed using the same `k` as `A = k × G`

This lets you **verify offline** that the mint legitimately signed your token, using only the mint's public key `A`.

## The Problem: DLEQ Proves Token Validity, Not Tally Inclusion

DLEQ proves:
- ✅ Your token was signed by the mint
- ✅ The signature is cryptographically valid
- ❌ **Your vote was included in the final count**

## The Solution: Add Merkle Tree Inclusion Proofs

### Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│  1. TOKEN ACQUISITION                                 │
│  ──────────────────────────────────────────────────────────────────┤
│  Voter requests tokens from mint                         │
│  Mint blind-signs with DLEQ proof                          │
│  Voter receives: Proof {secret, C, amount} + DLEQ {e, s, r}  │
│  Voter can verify DLEQ offline: signature is valid        │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│  2. VOTE CASTING                                          │
│  ──────────────────────────────────────────────────────────────────┤
│  Voter sends to mint:                                    │
│    - Proof {secret, C, amount}                             │
│    - Vote option: "RED"                                  │
│    - WITHOUT the blinding factor r!                       │
│                                                          │
│  Mint:                                                    │
│    1. Verifies proof signature (DLEQ)                       │
│    2. Marks token as spent (prevent double-voting)             │
│    3. Records: (hash(C), "RED", amount)                      │
│    4. Returns: receipt with hash(C)                    │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│  3. TALLY FINALIZATION                                   │
│  ──────────────────────────────────────────────────────────────────┤
│  After voting closes, mint:                              │
│    1. Builds Merkle tree from all recorded votes              │
│    2. Publishes:                                        │
│       - Merkle root (cryptographic commitment)               │
│       - Full vote list: [{hash(C), option, amount}, ...]          │
│       - Final tally: {RED: 150, BLUE: 80}                  │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│  4. VERIFICATION (Voter)                                │
│  ──────────────────────────────────────────────────────────────────┤
│  Voter receives published tally:                           │
│                                                          │
│  Verification steps:                                    │
│    1. Find hash(C) in published vote list                  │
│       → "My vote is there!" ✓                              │
│                                                          │
│    2. Check option and amount match what I voted              │
│       → "My vote was recorded correctly!" ✓               │
│                                                          │
│    3. (Optional) Verify Merkle proof to root                   │
│       → "The list is cryptographically committed!" ✓           │
│                                                          │
│    4. Recompute tally from published votes                   │
│       → "Tally matches announced results!" ✓                │
└─────────────────────────────────────────────────────────────────┘
```

---

### Why This Works

1. **DLEQ proves authenticity** — Your token was signed by the mint
2. **Merkle root proves commitment** — The mint can't change the vote list after publishing
3. **Published list enables verification** — Anyone can verify the tally
4. **Blind signatures preserve anonymity** — Mint can't link your vote back to you

### Privacy Protection

**Critical step:** When casting your vote, you DO NOT send the blinding factor `r` from your DLEQ proof!

```rust
// WRONG - sends blinding factor
send Vote {
    proof: Proof { secret, C, amount },
    dleq: DLEQ { e, s, r },  // ← r allows mint to link back to original minting
    option: "RED"
}

// RIGHT - strips blinding factor
send Vote {
    proof: Proof { secret, C, amount },
    dleq: DLEQ { e, s },     // ← No r = unlinkability preserved
    option: "RED"
}
```

Without `r`, the mint cannot link your vote to the original token minting session.

### Implementation Requirements

1. **Vote casting endpoint**: Accept proof + vote option, verify, mark spent, record vote
2. **Vote storage**: Append-only list of `(hash(C), option, amount)`
3. **Finalization endpoint**: Build Merkle tree, publish root + vote list
4. **Voter receipt**: Return `hash(C)` and Merkle proof at cast time
5. **Public audit**: Anyone can verify tally from published data

### Limitations

1. **Vote-buying risk**: Voters can prove how they voted (via Merkle proof), which could enable coercion
2. **Mint trust**: The mint could add fake votes. For full trustlessness, need threshold mint or publish blinded signatures at issuance

---

## Summary

| Question | Answer |
|----------|--------|
| Can DLEQ alone prove vote inclusion? | **No** — DLEQ proves token validity, not tally inclusion |
| What additional mechanism is needed? | **Merkle Tree + Published Vote Ledger** |
| How does this preserve anonymity? | **Strip blinding factor when casting vote** |
| How does this enable verification? | **Voter finds their hash(C) in published list** |
| Is this production-ready? | **Yes** — Helios uses this pattern successfully |
| Implementation effort? | **Done** — See `cdk-common::merkle` and `cdk-common::vote_ledger` |

---

## Implementation

The vote verifiability system is implemented in CDK:

### Merkle Tree (`cdk-common::merkle`)

```rust
// Create tree from vote leaf hashes
let tree = MerkleTree::new(&leaf_hashes);

// Get root commitment
let root = tree.root();

// Generate proof for vote at index i
let proof = tree.proof(i)?;

// Verify proof
assert!(MerkleTree::verify(&proof, leaf, root));
```

### Vote Ledger (`cdk-common::vote_ledger`)

```rust
let mut ledger = VerifiableVoteLedger::new("Red vs Blue".to_string());

// Record vote with commitment (sha256 of proof signature C)
let index = ledger.record_vote(commitment, "RED", 100)?;

// Finalize and get Merkle root
let root = ledger.finalize()?;

// Generate proof for voter
let (entry, proof) = ledger.proof(index)?;

// Voter verifies:
assert!(VerifiableVoteLedger::verify_proof(&proof, &entry, root));
```

### API Endpoint

The vote tally can be published at:
```
GET /v1/votes
GET /v1/votes/{commitment}/proof
```

### Verification Flow for Voters

1. **During vote**: Receive `commitment = sha256(C)` and `index`
2. **After finalization**: Fetch published tally from `/v1/votes`
3. **Verify inclusion**: 
   - Find `commitment` in published vote list
   - Check `option` and `amount` match
4. **Verify integrity** (optional):
   - Fetch Merkle proof from `/v1/votes/{commitment}/proof`
   - Verify proof against published root
