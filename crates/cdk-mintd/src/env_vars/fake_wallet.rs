//! FakeWallet environment variables

use std::env;

use cdk::nuts::CurrencyUnit;

use crate::config::FakeWallet;

// Fake Wallet environment variables
pub const ENV_FAKE_WALLET_SUPPORTED_UNITS: &str = "CDK_MINTD_FAKE_WALLET_SUPPORTED_UNITS";
pub const ENV_FAKE_WALLET_FEE_PERCENT: &str = "CDK_MINTD_FAKE_WALLET_FEE_PERCENT";
pub const ENV_FAKE_WALLET_RESERVE_FEE_MIN: &str = "CDK_MINTD_FAKE_WALLET_RESERVE_FEE_MIN";
pub const ENV_FAKE_WALLET_MIN_DELAY: &str = "CDK_MINTD_FAKE_WALLET_MIN_DELAY";
pub const ENV_FAKE_WALLET_MAX_DELAY: &str = "CDK_MINTD_FAKE_WALLET_MAX_DELAY";
pub const ENV_FAKE_WALLET_MANUAL_APPROVAL_INCOMING: &str =
    "CDK_MINTD_FAKE_WALLET_MANUAL_APPROVAL_INCOMING";
pub const ENV_FAKE_WALLET_MANUAL_APPROVAL_OUTGOING: &str =
    "CDK_MINTD_FAKE_WALLET_MANUAL_APPROVAL_OUTGOING";
pub const ENV_FAKE_WALLET_ACCEPT_ARBITRARY_MELT: &str =
    "CDK_MINTD_FAKE_WALLET_ACCEPT_ARBITRARY_MELT";
pub const ENV_FAKE_WALLET_ARBITRARY_MELT_FEE: &str = "CDK_MINTD_FAKE_WALLET_ARBITRARY_MELT_FEE";

impl FakeWallet {
    pub fn from_env(mut self) -> Self {
        // Supported Units - expects comma-separated list
        if let Ok(units_str) = env::var(ENV_FAKE_WALLET_SUPPORTED_UNITS) {
            if let Ok(units) = units_str
                .split(',')
                .map(|s| s.trim().parse())
                .collect::<Result<Vec<CurrencyUnit>, _>>()
            {
                self.supported_units = units;
            }
        }

        if let Ok(fee_str) = env::var(ENV_FAKE_WALLET_FEE_PERCENT) {
            if let Ok(fee) = fee_str.parse() {
                self.fee_percent = fee;
            }
        }

        if let Ok(reserve_fee_str) = env::var(ENV_FAKE_WALLET_RESERVE_FEE_MIN) {
            if let Ok(reserve_fee) = reserve_fee_str.parse::<u64>() {
                self.reserve_fee_min = reserve_fee.into();
            }
        }

        if let Ok(min_delay_str) = env::var(ENV_FAKE_WALLET_MIN_DELAY) {
            if let Ok(min_delay) = min_delay_str.parse() {
                self.min_delay_time = min_delay;
            }
        }

        if let Ok(max_delay_str) = env::var(ENV_FAKE_WALLET_MAX_DELAY) {
            if let Ok(max_delay) = max_delay_str.parse() {
                self.max_delay_time = max_delay;
            }
        }

        if let Ok(manual_incoming_str) = env::var(ENV_FAKE_WALLET_MANUAL_APPROVAL_INCOMING) {
            if let Ok(manual_incoming) = manual_incoming_str.parse::<bool>() {
                self.manual_approval_incoming = manual_incoming;
            }
        }

        if let Ok(manual_outgoing_str) = env::var(ENV_FAKE_WALLET_MANUAL_APPROVAL_OUTGOING) {
            if let Ok(manual_outgoing) = manual_outgoing_str.parse::<bool>() {
                self.manual_approval_outgoing = manual_outgoing;
            }
        }

        if let Ok(accept_arbitrary_melt_str) = env::var(ENV_FAKE_WALLET_ACCEPT_ARBITRARY_MELT) {
            if let Ok(accept_arbitrary_melt) = accept_arbitrary_melt_str.parse::<bool>() {
                self.accept_arbitrary_melt_requests = accept_arbitrary_melt;
            }
        }

        if let Ok(arbitrary_melt_fee_str) = env::var(ENV_FAKE_WALLET_ARBITRARY_MELT_FEE) {
            if let Ok(arbitrary_melt_fee_sat) = arbitrary_melt_fee_str.parse::<u64>() {
                self.arbitrary_melt_fee_sat = arbitrary_melt_fee_sat;
            }
        }

        self
    }
}
