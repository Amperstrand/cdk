//! NUT-08: Lightning fee return
//!
//! <https://github.com/cashubtc/nuts/blob/main/08.md>

use super::nut05::{MeltQuoteCustomResponse, MeltRequest};
use super::nut23::MeltQuoteBolt11Response;
use crate::Amount;

impl<Q> MeltRequest<Q> {
    /// Total output [`Amount`]
    // NUT #08: which ensures that there is at least one output if there is any fee
    pub fn output_amount(&self) -> Option<Amount> {
        self.outputs()
            .as_ref()
            .and_then(|o| Amount::try_sum(o.iter().map(|proof| proof.amount)).ok())
    }
}

impl<Q> MeltQuoteBolt11Response<Q> {
    /// Total change [`Amount`]
    // NUT #08: return the all blank signatures with a value greater than 0 in the same order as the blank outputs were received and should omit all blind signatures with value 0
    // NUT #08: that account for the overpaid fees
    pub fn change_amount(&self) -> Option<Amount> {
        self.change
            .as_ref()
            .and_then(|o| Amount::try_sum(o.iter().map(|proof| proof.amount)).ok())
    }
}

impl<Q> MeltQuoteCustomResponse<Q> {
    /// Total change [`Amount`]
    pub fn change_amount(&self) -> Option<Amount> {
        self.change
            .as_ref()
            .and_then(|o| Amount::try_sum(o.iter().map(|proof| proof.amount)).ok())
    }
}
