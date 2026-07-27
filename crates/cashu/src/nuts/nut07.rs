//! NUT-07: Spendable Check
//!
//! <https://github.com/cashubtc/nuts/blob/main/07.md>

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::nut01::PublicKey;
use super::Witness;

/// NUT07 Error
#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    /// Unknown State error
    #[error("Unknown state")]
    UnknownState,
}

/// State of Proof
// NUT #07: A proof can be in one of the following states
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum State {
    /// Spent
    // NUT #07: if it has been redeemed and its secret is in the list of spent secrets of the mint.
    Spent,
    /// Unspent
    // NUT #07: if it has not been spent yet
    Unspent,
    /// Pending
    ///
    /// Currently being used in a transaction i.e. melt in progress
    // NUT #07: if it is being processed in a transaction
    // NUT #07: proof cannot be used in another transaction until it is
    // NUT #07: remember which proofs are currently...to avoid reuse of the same token in multiple concurrent transactions
    Pending,
    /// Reserved
    ///
    /// Proof is reserved for future token creation
    Reserved,
    /// Pending spent (i.e., spent but not yet swapped by receiver)
    PendingSpent,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Spent => "SPENT",
            Self::Unspent => "UNSPENT",
            Self::Pending => "PENDING",
            Self::Reserved => "RESERVED",
            Self::PendingSpent => "PENDING_SPENT",
        };

        write!(f, "{s}")
    }
}

impl FromStr for State {
    type Err = Error;

    fn from_str(state: &str) -> Result<Self, Self::Err> {
        match state {
            "SPENT" => Ok(Self::Spent),
            "UNSPENT" => Ok(Self::Unspent),
            "PENDING" => Ok(Self::Pending),
            "RESERVED" => Ok(Self::Reserved),
            "PENDING_SPENT" => Ok(Self::PendingSpent),
            _ => Err(Error::UnknownState),
        }
    }
}

/// Check spendable request [NUT-07]
// NUT #07: are the hexadecimal representation of the compressed point
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckStateRequest {
    /// Y's of the proofs to check
    #[serde(rename = "Ys")]
    pub ys: Vec<PublicKey>,
}

/// Proof state [NUT-07]
// NUT #07: is the serialized witness data that was used to spend the
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofState {
    /// Y of proof
    #[serde(rename = "Y")]
    pub y: PublicKey,
    /// State of proof
    pub state: State,
    /// Witness data if it is supplied
    pub witness: Option<Witness>,
}

impl From<(PublicKey, State)> for ProofState {
    fn from(value: (PublicKey, State)) -> Self {
        Self {
            y: value.0,
            state: value.1,
            witness: None,
        }
    }
}

/// Check Spendable Response [NUT-07]
// NUT #07: MUST be returned in the same order as the corresponding
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckStateResponse {
    /// Proof states
    pub states: Vec<ProofState>,
}
