//! Verifiable Vote Ledger with Merkle Proofs
//!
//! This module provides a vote ledger that tracks individual votes and can
//! generate Merkle proofs for vote verification. Voters can verify their
//! vote was included in the final tally.

use bitcoin::hashes::sha256::Hash as Sha256Hash;
use bitcoin::hashes::{Hash, HashEngine};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::merkle::{hash_bytes, MerkleProof, MerkleTree};

/// A single recorded vote entry
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoteEntry {
    /// Hash commitment of the proof signature (sha256(C))
    pub commitment: [u8; 32],
    /// The voting option (e.g., "RED", "BLUE")
    pub option: String,
    /// Vote weight in satoshis
    pub amount_sat: u64,
    /// Unix timestamp when vote was cast
    pub timestamp: u64,
    /// Index in the vote list (for Merkle proof generation)
    #[serde(default)]
    pub index: usize,
}

impl VoteEntry {
    /// Create a new vote entry
    pub fn new(commitment: [u8; 32], option: String, amount_sat: u64, timestamp: u64) -> Self {
        Self {
            commitment,
            option,
            amount_sat,
            timestamp,
            index: 0,
        }
    }

    /// Hash this entry for Merkle tree inclusion
    pub fn leaf_hash(&self) -> [u8; 32] {
        let mut engine = Sha256Hash::engine();
        engine.input(&self.commitment);
        engine.input(self.option.as_bytes());
        engine.input(&self.amount_sat.to_le_bytes());
        engine.input(&self.timestamp.to_le_bytes());
        Sha256Hash::from_engine(engine).to_byte_array()
    }
}

/// Tally for a single voting option
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoteTally {
    /// The voting option
    pub option: String,
    /// Total satoshis melted to this option (vote weight)
    pub total_amount: u64,
    /// Number of individual votes cast
    pub vote_count: u64,
}

/// The complete verifiable vote ledger
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiableVoteLedger {
    /// Topic/title of the voting session
    pub topic: String,
    /// Unix timestamp when voting started
    pub created_at: u64,
    /// All recorded vote entries
    pub entries: Vec<VoteEntry>,
    /// Aggregated tallies per option
    pub tallies: HashMap<String, VoteTally>,
    /// Merkle root (computed on finalization)
    #[serde(default)]
    pub merkle_root: Option<[u8; 32]>,
    /// Whether voting has been finalized
    #[serde(default)]
    pub finalized: bool,
}

impl VerifiableVoteLedger {
    /// Create a new empty vote ledger
    pub fn new(topic: String) -> Self {
        Self {
            topic,
            created_at: unix_timestamp_now(),
            entries: Vec::new(),
            tallies: HashMap::new(),
            merkle_root: None,
            finalized: false,
        }
    }

    /// Record a vote
    pub fn record_vote(
        &mut self,
        commitment: [u8; 32],
        option: &str,
        amount_sat: u64,
    ) -> Result<usize, VoteLedgerError> {
        if self.finalized {
            return Err(VoteLedgerError::AlreadyFinalized);
        }

        let normalized_option = option.to_uppercase();

        let index = self.entries.len();
        let entry = VoteEntry::new(
            commitment,
            normalized_option.clone(),
            amount_sat,
            unix_timestamp_now(),
        );
        self.entries.push(entry);

        let tally = self
            .tallies
            .entry(normalized_option.clone())
            .or_insert(VoteTally {
                option: normalized_option,
                total_amount: 0,
                vote_count: 0,
            });
        tally.total_amount += amount_sat;
        tally.vote_count += 1;

        Ok(index)
    }

    /// Get the total number of votes
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the ledger is empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get total vote weight across all options
    pub fn total_amount(&self) -> u64 {
        self.tallies.values().map(|t| t.total_amount).sum()
    }

    /// Get total vote count across all options
    pub fn total_count(&self) -> u64 {
        self.tallies.values().map(|t| t.vote_count).sum()
    }

    /// Finalize the vote ledger and compute Merkle root
    pub fn finalize(&mut self) -> Result<[u8; 32], VoteLedgerError> {
        if self.finalized {
            return Err(VoteLedgerError::AlreadyFinalized);
        }

        if self.entries.is_empty() {
            self.merkle_root = Some([0u8; 32]);
            self.finalized = true;
            return Ok([0u8; 32]);
        }

        let leaves: Vec<[u8; 32]> = self.entries.iter().map(|e| e.leaf_hash()).collect();
        let tree = MerkleTree::new(&leaves);
        let root = tree.root();

        self.merkle_root = Some(root);
        self.finalized = true;

        Ok(root)
    }

    /// Generate a Merkle proof for the vote at the given index
    pub fn proof(&self, index: usize) -> Result<(VoteEntry, MerkleProof), VoteLedgerError> {
        if index >= self.entries.len() {
            return Err(VoteLedgerError::InvalidIndex);
        }

        let Some(root) = self.merkle_root else {
            return Err(VoteLedgerError::NotFinalized);
        };

        let leaves: Vec<[u8; 32]> = self.entries.iter().map(|e| e.leaf_hash()).collect();
        let tree = MerkleTree::new(&leaves);

        let proof = tree
            .proof(index)
            .ok_or(VoteLedgerError::ProofGenerationFailed)?;

        let entry = self.entries[index].clone();

        Ok((entry, proof))
    }

    /// Verify a Merkle proof
    pub fn verify_proof(proof: &MerkleProof, entry: &VoteEntry, root: [u8; 32]) -> bool {
        let leaf = entry.leaf_hash();
        MerkleTree::verify(proof, leaf, root)
    }

    /// Get results for a specific option
    pub fn get_tally(&self, option: &str) -> Option<&VoteTally> {
        self.tallies.get(&option.to_uppercase())
    }

    /// Get all tallies sorted by total amount (descending)
    pub fn sorted_tallies(&self) -> Vec<&VoteTally> {
        let mut tallies: Vec<_> = self.tallies.values().collect();
        tallies.sort_by(|a, b| b.total_amount.cmp(&a.total_amount));
        tallies
    }

    /// Reset the ledger for a new voting session
    pub fn reset(&mut self) {
        self.entries.clear();
        self.tallies.clear();
        self.merkle_root = None;
        self.finalized = false;
        self.created_at = unix_timestamp_now();
    }
}

/// Error type for vote ledger operations
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VoteLedgerError {
    #[error("Vote ledger already finalized")]
    AlreadyFinalized,
    #[error("Vote ledger not finalized")]
    NotFinalized,
    #[error("Invalid vote index")]
    InvalidIndex,
    #[error("Failed to generate Merkle proof")]
    ProofGenerationFailed,
}

fn unix_timestamp_now() -> u64 {
    match web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH) {
        Ok(duration) => duration.as_secs(),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_ledger() {
        let ledger = VerifiableVoteLedger::new("Test Vote".to_string());
        assert!(ledger.is_empty());
        assert_eq!(ledger.len(), 0);
        assert_eq!(ledger.topic, "Test Vote");
    }

    #[test]
    fn test_record_vote() {
        let mut ledger = VerifiableVoteLedger::new("Test".to_string());
        let commitment = [1u8; 32];

        let index = ledger.record_vote(commitment, "RED", 100).unwrap();
        assert_eq!(index, 0);
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger.total_amount(), 100);
        assert_eq!(ledger.total_count(), 1);

        let tally = ledger.get_tally("RED").unwrap();
        assert_eq!(tally.total_amount, 100);
        assert_eq!(tally.vote_count, 1);
    }

    #[test]
    fn test_multiple_votes() {
        let mut ledger = VerifiableVoteLedger::new("Test".to_string());

        ledger.record_vote([1u8; 32], "RED", 100).unwrap();
        ledger.record_vote([2u8; 32], "BLUE", 200).unwrap();
        ledger.record_vote([3u8; 32], "RED", 50).unwrap();

        assert_eq!(ledger.len(), 3);
        assert_eq!(ledger.total_amount(), 350);
        assert_eq!(ledger.total_count(), 3);

        let red_tally = ledger.get_tally("RED").unwrap();
        assert_eq!(red_tally.total_amount, 150);
        assert_eq!(red_tally.vote_count, 2);

        let blue_tally = ledger.get_tally("BLUE").unwrap();
        assert_eq!(blue_tally.total_amount, 200);
        assert_eq!(blue_tally.vote_count, 1);
    }

    #[test]
    fn test_finalize_and_proof() {
        let mut ledger = VerifiableVoteLedger::new("Test".to_string());

        ledger.record_vote([1u8; 32], "RED", 100).unwrap();
        ledger.record_vote([2u8; 32], "BLUE", 200).unwrap();

        let root = ledger.finalize().unwrap();
        assert!(ledger.finalized);
        assert!(ledger.merkle_root.is_some());

        let (entry, proof) = ledger.proof(0).unwrap();
        assert!(VerifiableVoteLedger::verify_proof(&proof, &entry, root));

        let (entry2, proof2) = ledger.proof(1).unwrap();
        assert!(VerifiableVoteLedger::verify_proof(&proof2, &entry2, root));
    }

    #[test]
    fn test_cannot_vote_after_finalize() {
        let mut ledger = VerifiableVoteLedger::new("Test".to_string());
        ledger.record_vote([1u8; 32], "RED", 100).unwrap();
        ledger.finalize().unwrap();

        let result = ledger.record_vote([2u8; 32], "BLUE", 200);
        assert!(matches!(result, Err(VoteLedgerError::AlreadyFinalized)));
    }

    #[test]
    fn test_sorted_tallies() {
        let mut ledger = VerifiableVoteLedger::new("Test".to_string());

        ledger.record_vote([1u8; 32], "RED", 100).unwrap();
        ledger.record_vote([2u8; 32], "BLUE", 300).unwrap();
        ledger.record_vote([3u8; 32], "GREEN", 200).unwrap();

        let sorted = ledger.sorted_tallies();
        assert_eq!(sorted[0].option, "BLUE");
        assert_eq!(sorted[1].option, "GREEN");
        assert_eq!(sorted[2].option, "RED");
    }

    #[test]
    fn test_invalid_proof() {
        let mut ledger = VerifiableVoteLedger::new("Test".to_string());

        ledger.record_vote([1u8; 32], "RED", 100).unwrap();
        let root = ledger.finalize().unwrap();

        let (mut entry, proof) = ledger.proof(0).unwrap();
        entry.amount_sat = 999;

        assert!(!VerifiableVoteLedger::verify_proof(&proof, &entry, root));
    }

    #[test]
    fn test_case_insensitive() {
        let mut ledger = VerifiableVoteLedger::new("Test".to_string());

        ledger.record_vote([1u8; 32], "red", 100).unwrap();
        ledger.record_vote([2u8; 32], "RED", 100).unwrap();
        ledger.record_vote([3u8; 32], "Red", 100).unwrap();

        let tally = ledger.get_tally("red").unwrap();
        assert_eq!(tally.vote_count, 3);
        assert_eq!(tally.total_amount, 300);
    }
}
