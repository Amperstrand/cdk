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
pub const ENV_FAKE_WALLET_VOTING_ENABLED: &str = "CDK_MINTD_FAKE_WALLET_VOTING_ENABLED";
pub const ENV_FAKE_WALLET_VOTING_OPTIONS: &str = "CDK_MINTD_FAKE_WALLET_VOTING_OPTIONS";
pub const ENV_FAKE_WALLET_VOTING_TOPIC: &str = "CDK_MINTD_FAKE_WALLET_VOTING_TOPIC";
pub const ENV_FAKE_WALLET_VOTING_FEE_SAT: &str = "CDK_MINTD_FAKE_WALLET_VOTING_FEE_SAT";

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

        if let Ok(voting_enabled_str) = env::var(ENV_FAKE_WALLET_VOTING_ENABLED) {
            if let Ok(voting_enabled) = voting_enabled_str.parse::<bool>() {
                self.voting_enabled = voting_enabled;
            }
        }

        if let Ok(options_str) = env::var(ENV_FAKE_WALLET_VOTING_OPTIONS) {
            let options: Vec<String> = options_str
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_owned())
                .collect();

            if !options.is_empty() {
                self.voting_options = Some(options);
            }
        }

        if let Ok(topic) = env::var(ENV_FAKE_WALLET_VOTING_TOPIC) {
            let trimmed = topic.trim().to_owned();
            if !trimmed.is_empty() {
                self.voting_topic = Some(trimmed);
            }
        }

        if let Ok(voting_fee_sat_str) = env::var(ENV_FAKE_WALLET_VOTING_FEE_SAT) {
            if let Ok(voting_fee_sat) = voting_fee_sat_str.parse::<u64>() {
                self.voting_fee_sat = voting_fee_sat;
            }
        }

        self
    }
}
