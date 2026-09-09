//! In memory signatory
//!
//! Implements the Signatory trait from cdk-common to manage the key in-process, to be included
//! inside the mint to be executed as a single process.
//!
//! Even if it is embedded in the same process, the keys are not accessible from the outside of this
//! module, all communication is done through the Signatory trait and the signatory manager.

#[cfg(feature = "grpc")]

/// Legacy redemption policy for proof verification.
///
/// Three modes with distinct operational semantics:
///
/// - `Allow` — accept proofs under both canonical and legacy derivations
///   (per-keyset opt-in; honor issued claims, e.g. pre-0.15.1 nutshell tokens)
/// - `Observe` — accept canonical only, but compute legacy derivations and
///   log WARN on match (safe default: measures exposure without breaking anything)
/// - `RugPull` — accept canonical only, skip legacy checks entirely
///   (strictest and fastest; use when you are certain no legacy claims exist)
///
/// The intended lifecycle: start in `Observe`, watch the logs, promote
/// keysets with real legacy traffic to `Allow`, demote verified-clean
/// keysets to `RugPull` for performance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LegacyRedemptionMode {
    Allow,
    Observe,
    RugPull,
}

impl Default for LegacyRedemptionMode {
    fn default() -> Self {
        Self::Observe
    }
}


mod proto;

#[cfg(feature = "grpc")]
pub use proto::{
    client::SignatoryRpcClient,
    server::{start_grpc_server, start_grpc_server_with_incoming, SignatoryLoader},
};

mod common;

pub mod db_signatory;
pub mod embedded;
pub mod signatory;
