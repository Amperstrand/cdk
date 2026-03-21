//! Merkle Tree for Vote Verification
//!
//! This module provides a simple Merkle tree implementation for vote tally
//! verification. Each leaf is a hash of a vote entry, and the root commits
//! to the entire set of votes.

use bitcoin::hashes::sha256::Hash as Sha256Hash;
use bitcoin::hashes::{Hash, HashEngine};
use serde::{Deserialize, Serialize};

/// A Merkle tree for vote verification
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerkleTree {
    leaves: Vec<[u8; 32]>,
    root: [u8; 32],
}

/// A Merkle proof that a leaf is included in the tree
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MerkleProof {
    /// Index of the leaf in the tree
    pub index: usize,
    /// Hashes along the path from leaf to root
    pub path: Vec<[u8; 32]>,
    /// Direction flags: true = sibling is on the right
    pub directions: Vec<bool>,
}

impl MerkleTree {
    /// Create a new Merkle tree from leaf hashes
    pub fn new(leaves: &[[u8; 32]]) -> Self {
        if leaves.is_empty() {
            return Self {
                leaves: vec![],
                root: [0u8; 32],
            };
        }

        let root = compute_root(leaves);
        Self {
            leaves: leaves.to_vec(),
            root,
        }
    }

    /// Get the Merkle root
    pub fn root(&self) -> [u8; 32] {
        self.root
    }

    /// Get the number of leaves
    pub fn len(&self) -> usize {
        self.leaves.len()
    }

    /// Check if the tree is empty
    pub fn is_empty(&self) -> bool {
        self.leaves.is_empty()
    }

    /// Generate a Merkle proof for the leaf at the given index
    pub fn proof(&self, index: usize) -> Option<MerkleProof> {
        if index >= self.leaves.len() || self.leaves.is_empty() {
            return None;
        }

        let mut path = vec![];
        let mut directions = vec![];
        let mut current_index = index;
        let mut current_level = self.leaves.clone();

        while current_level.len() > 1 {
            let sibling_index = if current_index % 2 == 0 {
                current_index + 1
            } else {
                current_index - 1
            };

            if sibling_index < current_level.len() {
                path.push(current_level[sibling_index]);
                directions.push(current_index % 2 == 0);
            } else if current_index % 2 == 0 {
                path.push(current_level[current_index]);
                directions.push(true);
            }

            current_level = hash_level(&current_level);
            current_index /= 2;
        }

        Some(MerkleProof {
            index,
            path,
            directions,
        })
    }

    /// Verify a Merkle proof
    pub fn verify(proof: &MerkleProof, leaf: [u8; 32], root: [u8; 32]) -> bool {
        if proof.path.len() != proof.directions.len() {
            return false;
        }

        let mut current = leaf;
        for (sibling, is_right) in proof.path.iter().zip(proof.directions.iter()) {
            current = if *is_right {
                hash_pair(&current, sibling)
            } else {
                hash_pair(sibling, &current)
            };
        }

        current == root
    }
}

/// Compute the Merkle root from a list of leaves
fn compute_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    if leaves.is_empty() {
        return [0u8; 32];
    }

    if leaves.len() == 1 {
        return leaves[0];
    }

    let mut current_level = leaves.to_vec();

    while current_level.len() > 1 {
        current_level = hash_level(&current_level);
    }

    current_level[0]
}

/// Hash pairs of nodes at a given level
fn hash_level(level: &[[u8; 32]]) -> Vec<[u8; 32]> {
    let mut next_level = vec![];

    for chunk in level.chunks(2) {
        let hash = if chunk.len() == 2 {
            hash_pair(&chunk[0], &chunk[1])
        } else {
            chunk[0]
        };
        next_level.push(hash);
    }

    next_level
}

/// Hash two 32-byte values together
pub fn hash_pair(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut engine = Sha256Hash::engine();
    engine.input(left);
    engine.input(right);
    Sha256Hash::from_engine(engine).to_byte_array()
}

/// Hash arbitrary bytes to a 32-byte array
pub fn hash_bytes(data: &[u8]) -> [u8; 32] {
    Sha256Hash::hash(data).to_byte_array()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_tree() {
        let tree = MerkleTree::new(&[]);
        assert!(tree.is_empty());
        assert_eq!(tree.len(), 0);
        assert_eq!(tree.root(), [0u8; 32]);
    }

    #[test]
    fn test_single_leaf() {
        let leaf = [1u8; 32];
        let tree = MerkleTree::new(&[leaf]);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree.root(), leaf);
    }

    #[test]
    fn test_two_leaves() {
        let leaf1 = [1u8; 32];
        let leaf2 = [2u8; 32];
        let tree = MerkleTree::new(&[leaf1, leaf2]);

        let expected_root = hash_pair(&leaf1, &leaf2);
        assert_eq!(tree.root(), expected_root);
    }

    #[test]
    fn test_proof_single_leaf() {
        let leaf = [1u8; 32];
        let tree = MerkleTree::new(&[leaf]);

        let proof = tree.proof(0).unwrap();
        assert!(proof.path.is_empty());
        assert!(proof.directions.is_empty());

        assert!(MerkleTree::verify(&proof, leaf, tree.root()));
    }

    #[test]
    fn test_proof_two_leaves() {
        let leaf1 = [1u8; 32];
        let leaf2 = [2u8; 32];
        let tree = MerkleTree::new(&[leaf1, leaf2]);

        let proof1 = tree.proof(0).unwrap();
        assert!(MerkleTree::verify(&proof1, leaf1, tree.root()));

        let proof2 = tree.proof(1).unwrap();
        assert!(MerkleTree::verify(&proof2, leaf2, tree.root()));
    }

    #[test]
    fn test_proof_four_leaves() {
        let leaves: Vec<[u8; 32]> = (0..4).map(|i| [i as u8; 32]).collect();
        let tree = MerkleTree::new(&leaves);

        for (i, leaf) in leaves.iter().enumerate() {
            let proof = tree.proof(i).unwrap();
            assert!(
                MerkleTree::verify(&proof, *leaf, tree.root()),
                "Proof verification failed for leaf {}",
                i
            );
        }
    }

    #[test]
    fn test_proof_invalid_leaf() {
        let leaves: Vec<[u8; 32]> = (0..4).map(|i| [i as u8; 32]).collect();
        let tree = MerkleTree::new(&leaves);

        let proof = tree.proof(0).unwrap();
        let wrong_leaf = [99u8; 32];
        assert!(!MerkleTree::verify(&proof, wrong_leaf, tree.root()));
    }

    #[test]
    fn test_proof_invalid_root() {
        let leaves: Vec<[u8; 32]> = (0..4).map(|i| [i as u8; 32]).collect();
        let tree = MerkleTree::new(&leaves);

        let proof = tree.proof(0).unwrap();
        let wrong_root = [99u8; 32];
        assert!(!MerkleTree::verify(&proof, leaves[0], wrong_root));
    }

    #[test]
    fn test_odd_number_of_leaves() {
        let leaves: Vec<[u8; 32]> = (0..3).map(|i| [i as u8; 32]).collect();
        let tree = MerkleTree::new(&leaves);

        for (i, leaf) in leaves.iter().enumerate() {
            let proof = tree.proof(i).unwrap();
            assert!(MerkleTree::verify(&proof, *leaf, tree.root()));
        }
    }
}
