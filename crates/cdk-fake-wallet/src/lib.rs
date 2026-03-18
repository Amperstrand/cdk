//! CDK Fake LN Backend
//!
//! Used for testing where quotes are auto filled.
//!
//! The fake wallet now includes a secondary repayment system that continuously repays any-amount
//! invoices (amount = 0) at random intervals between 30 seconds and 3 minutes to simulate
//! real-world behavior where invoices might get multiple payments. Payments continue to be
//! processed until they are evicted from the queue when the queue reaches its maximum size
//! (default 100 items). This is in addition to the original immediate payment processing
//! which is maintained for all invoice types.

#![doc = include_str!("../README.md")]

use std::cmp::max;
use std::collections::{HashMap, HashSet, VecDeque};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bitcoin::hashes::{sha256, Hash};
use bitcoin::secp256k1::{Secp256k1, SecretKey};
use cdk_common::amount::Amount;
use cdk_common::common::FeeReserve;
use cdk_common::ensure_cdk;
use cdk_common::nuts::{CurrencyUnit, MeltOptions, MeltQuoteState};
use cdk_common::payment::{
    self, CreateIncomingPaymentResponse, Event, IncomingPaymentOptions, MakePaymentResponse,
    MintPayment, OutgoingPaymentOptions, PaymentIdentifier, PaymentQuoteResponse, SettingsResponse,
    WaitPaymentResponse,
};
use error::Error;
use futures::stream::StreamExt;
use futures::Stream;
use lightning::offers::offer::OfferBuilder;
use lightning_invoice::{Bolt11Invoice, Currency, InvoiceBuilder, PaymentSecret};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tokio::time;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tracing::instrument;
use uuid::Uuid;

pub mod error;

/// Default maximum size for the secondary repayment queue
const DEFAULT_REPAY_QUEUE_MAX_SIZE: usize = 100;

/// Payment state entry containing the melt quote state and amount spent
type PaymentStateEntry = (MeltQuoteState, Amount<CurrencyUnit>);

#[derive(Debug, Clone)]
struct PendingIncomingPayment {
    payment_amount: Amount<CurrencyUnit>,
    is_any_amount: bool,
}

/// A vote tally for a single voting option.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteTally {
    /// The voting option (e.g., "RED", "BLUE")
    pub option: String,
    /// Total satoshis melted to this option (vote weight)
    pub total_amount: u64,
    /// Number of individual votes cast
    pub vote_count: u64,
}

/// The complete vote ledger tracking all options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteLedger {
    /// Map of option -> tally
    pub options: HashMap<String, VoteTally>,
    /// Topic/title of the voting session
    pub topic: String,
    /// Unix timestamp when voting started
    pub created_at: u64,
}

fn unix_timestamp_now() -> u64 {
    match web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH) {
        Ok(duration) => duration.as_secs(),
        Err(_) => 0,
    }
}

/// Cache duration for exchange rate (5 minutes)
const RATE_CACHE_DURATION: Duration = Duration::from_secs(300);

/// Mempool.space prices API response structure
#[derive(Debug, Deserialize)]
struct MempoolPricesResponse {
    #[serde(rename = "USD")]
    usd: f64,
    #[serde(rename = "EUR")]
    eur: f64,
}

/// Exchange rate cache with built-in fallback rates
#[derive(Debug, Clone)]
struct ExchangeRateCache {
    rates: Arc<Mutex<Option<(MempoolPricesResponse, Instant)>>>,
}

impl ExchangeRateCache {
    fn new() -> Self {
        Self {
            rates: Arc::new(Mutex::new(None)),
        }
    }

    /// Get current BTC rate for the specified currency with caching and fallback
    async fn get_btc_rate(&self, currency: &CurrencyUnit) -> Result<f64, Error> {
        // Return cached rate if still valid
        {
            let cached_rates = self.rates.lock().await;
            if let Some((rates, timestamp)) = &*cached_rates {
                if timestamp.elapsed() < RATE_CACHE_DURATION {
                    return Self::rate_for_currency(rates, currency);
                }
            }
        }

        // Try to fetch fresh rates, fallback on error
        match self.fetch_fresh_rate(currency).await {
            Ok(rate) => Ok(rate),
            Err(e) => {
                tracing::warn!(
                    "Failed to fetch exchange rates, using fallback for {:?}: {}",
                    currency,
                    e
                );
                Self::fallback_rate(currency)
            }
        }
    }

    /// Fetch fresh rate and update cache
    async fn fetch_fresh_rate(&self, currency: &CurrencyUnit) -> Result<f64, Error> {
        let url = "https://mempool.space/api/v1/prices";
        let response: MempoolPricesResponse = cdk_common::fetch(url)
            .await
            .map_err(|_| Error::UnknownInvoiceAmount)?;

        let rate = Self::rate_for_currency(&response, currency)?;
        *self.rates.lock().await = Some((response, Instant::now()));
        Ok(rate)
    }

    fn rate_for_currency(
        rates: &MempoolPricesResponse,
        currency: &CurrencyUnit,
    ) -> Result<f64, Error> {
        match currency {
            CurrencyUnit::Usd => Ok(rates.usd),
            CurrencyUnit::Eur => Ok(rates.eur),
            _ => Err(Error::UnknownInvoiceAmount),
        }
    }

    fn fallback_rate(currency: &CurrencyUnit) -> Result<f64, Error> {
        match currency {
            CurrencyUnit::Usd => Ok(110_000.0), // $110k per BTC
            CurrencyUnit::Eur => Ok(95_000.0),  // €95k per BTC
            _ => Err(Error::UnknownInvoiceAmount),
        }
    }
}

async fn convert_currency_amount(
    amount: u64,
    from_unit: &CurrencyUnit,
    target_unit: &CurrencyUnit,
    rate_cache: &ExchangeRateCache,
) -> Result<Amount<CurrencyUnit>, Error> {
    use CurrencyUnit::*;

    // Try basic unit conversion first (handles SAT/MSAT and same-unit conversions)
    if let Ok(converted) = Amount::new(amount, from_unit.clone()).convert_to(target_unit) {
        return Ok(converted);
    }

    // Handle fiat <-> bitcoin conversions that require exchange rates
    match (from_unit, target_unit) {
        // Fiat to Bitcoin conversions
        (Usd | Eur, Sat) => {
            let rate = rate_cache.get_btc_rate(from_unit).await?;
            let fiat_amount = amount as f64 / 100.0; // cents to dollars/euros
            Ok(Amount::new(
                (fiat_amount / rate * 100_000_000.0).round() as u64,
                target_unit.clone(),
            )) // to sats
        }
        (Usd | Eur, Msat) => {
            let rate = rate_cache.get_btc_rate(from_unit).await?;
            let fiat_amount = amount as f64 / 100.0; // cents to dollars/euros
            Ok(Amount::new(
                (fiat_amount / rate * 100_000_000_000.0).round() as u64,
                target_unit.clone(),
            )) // to msats
        }

        // Bitcoin to fiat conversions
        (Sat, Usd | Eur) => {
            let rate = rate_cache.get_btc_rate(target_unit).await?;
            let btc_amount = amount as f64 / 100_000_000.0; // sats to BTC
            Ok(Amount::new(
                (btc_amount * rate * 100.0).round() as u64,
                target_unit.clone(),
            )) // to cents
        }
        (Msat, Usd | Eur) => {
            let rate = rate_cache.get_btc_rate(target_unit).await?;
            let btc_amount = amount as f64 / 100_000_000_000.0; // msats to BTC
            Ok(Amount::new(
                (btc_amount * rate * 100.0).round() as u64,
                target_unit.clone(),
            )) // to cents
        }

        _ => Err(Error::UnknownInvoiceAmount), // Unsupported conversion
    }
}

/// Secondary repayment queue manager for any-amount invoices
#[derive(Debug, Clone)]
struct SecondaryRepaymentQueue {
    queue: Arc<Mutex<VecDeque<PaymentIdentifier>>>,
    max_size: usize,
    sender: tokio::sync::mpsc::Sender<WaitPaymentResponse>,
    unit: CurrencyUnit,
}

impl SecondaryRepaymentQueue {
    fn new(
        max_size: usize,
        sender: tokio::sync::mpsc::Sender<WaitPaymentResponse>,
        unit: CurrencyUnit,
    ) -> Self {
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let repayment_queue = Self {
            queue: queue.clone(),
            max_size,
            sender,
            unit,
        };

        // Start the background secondary repayment processor
        repayment_queue.start_secondary_repayment_processor();

        repayment_queue
    }

    /// Add a payment to the secondary repayment queue
    async fn enqueue_for_repayment(&self, payment: PaymentIdentifier) {
        let mut queue = self.queue.lock().await;

        // If queue is at max capacity, remove the oldest item
        if queue.len() >= self.max_size {
            if let Some(dropped) = queue.pop_front() {
                tracing::debug!(
                    "Secondary repayment queue at capacity, dropping oldest payment: {:?}",
                    dropped
                );
            }
        }

        queue.push_back(payment);
        tracing::debug!(
            "Added payment to secondary repayment queue, current size: {}",
            queue.len()
        );
    }

    /// Start the background task that randomly processes secondary repayments from the queue
    fn start_secondary_repayment_processor(&self) {
        let queue = self.queue.clone();
        let sender = self.sender.clone();
        let unit = self.unit.clone();

        tokio::spawn(async move {
            use bitcoin::secp256k1::rand::rngs::OsRng;
            use bitcoin::secp256k1::rand::Rng;
            let mut rng = OsRng;

            loop {
                // Wait for a random interval between 30 seconds and 3 minutes (180 seconds)
                let delay_secs = rng.gen_range(1..=3);
                time::sleep(time::Duration::from_secs(delay_secs)).await;

                // Try to process a random payment from the queue without removing it
                let payment_to_process = {
                    let q = queue.lock().await;
                    if q.is_empty() {
                        None
                    } else {
                        // Pick a random index from the queue but don't remove it
                        let index = rng.gen_range(0..q.len());
                        q.get(index).cloned()
                    }
                };

                if let Some(payment) = payment_to_process {
                    // Generate a random amount for this secondary payment (same range as initial payment: 1-1000)
                    let random_amount: u64 = rng.gen_range(1..=1000);

                    // Create amount based on unit, ensuring minimum of 1 sat worth
                    let secondary_amount = match &unit {
                        CurrencyUnit::Sat => Amount::new(random_amount, unit.clone()),
                        CurrencyUnit::Msat => {
                            Amount::new(u64::max(random_amount * 1000, 1000), unit.clone())
                        }
                        _ => Amount::new(u64::max(random_amount, 1), unit.clone()), // fallback
                    };

                    // Generate a unique payment identifier for this secondary payment
                    // We'll create a new payment hash by appending a timestamp and random bytes
                    use bitcoin::hashes::{sha256, Hash};
                    let mut random_bytes = [0u8; 16];
                    rng.fill(&mut random_bytes);
                    let timestamp = web_time::SystemTime::now()
                        .duration_since(web_time::UNIX_EPOCH)
                        .expect("System time before UNIX_EPOCH")
                        .as_nanos() as u64;

                    // Create a unique hash combining the original payment identifier, timestamp, and random bytes
                    let mut hasher_input = Vec::new();
                    hasher_input.extend_from_slice(payment.to_string().as_bytes());
                    hasher_input.extend_from_slice(&timestamp.to_le_bytes());
                    hasher_input.extend_from_slice(&random_bytes);

                    let unique_hash = sha256::Hash::hash(&hasher_input);
                    let unique_payment_id = PaymentIdentifier::PaymentHash(*unique_hash.as_ref());

                    tracing::info!(
                        "Processing secondary repayment: original={:?}, new_id={:?}, amount={}",
                        payment,
                        unique_payment_id,
                        secondary_amount
                    );

                    // Send the payment notification using the original payment identifier
                    // The mint will process this through the normal payment stream
                    let secondary_response = WaitPaymentResponse {
                        payment_identifier: payment.clone(),
                        payment_amount: secondary_amount,
                        payment_id: unique_payment_id.to_string(),
                    };

                    if let Err(e) = sender.send(secondary_response).await {
                        tracing::error!(
                            "Failed to send secondary repayment notification for {:?}: {}",
                            unique_payment_id,
                            e
                        );
                    }
                }
            }
        });
    }
}

/// Fake Wallet
#[derive(Clone, Debug)]
pub struct FakeWallet {
    fee_reserve: FeeReserve,
    sender: tokio::sync::mpsc::Sender<WaitPaymentResponse>,
    receiver: Arc<Mutex<Option<tokio::sync::mpsc::Receiver<WaitPaymentResponse>>>>,
    payment_states: Arc<Mutex<HashMap<String, PaymentStateEntry>>>,
    failed_payment_check: Arc<Mutex<HashSet<String>>>,
    payment_delay: u64,
    wait_invoice_cancel_token: CancellationToken,
    wait_invoice_is_active: Arc<AtomicBool>,
    incoming_payments: Arc<RwLock<HashMap<PaymentIdentifier, Vec<WaitPaymentResponse>>>>,
    pending_incoming_payments: Arc<Mutex<HashMap<PaymentIdentifier, PendingIncomingPayment>>>,
    manual_approval_incoming: bool,
    accept_voting_requests: bool,
    voting_fee_sat: u64,
    voting_options: Vec<String>,
    voting_topic: String,
    vote_ledger: Arc<Mutex<VoteLedger>>,
    unit: CurrencyUnit,
    secondary_repayment_queue: SecondaryRepaymentQueue,
    exchange_rate_cache: ExchangeRateCache,
}

impl FakeWallet {
    /// Create new [`FakeWallet`]
    pub fn new(
        fee_reserve: FeeReserve,
        payment_states: HashMap<String, PaymentStateEntry>,
        fail_payment_check: HashSet<String>,
        payment_delay: u64,
        unit: CurrencyUnit,
    ) -> Self {
        Self::new_with_repay_queue_size(
            fee_reserve,
            payment_states,
            fail_payment_check,
            payment_delay,
            unit,
            DEFAULT_REPAY_QUEUE_MAX_SIZE,
        )
    }

    /// Create new [`FakeWallet`] with custom secondary repayment queue size
    pub fn new_with_repay_queue_size(
        fee_reserve: FeeReserve,
        payment_states: HashMap<String, PaymentStateEntry>,
        fail_payment_check: HashSet<String>,
        payment_delay: u64,
        unit: CurrencyUnit,
        repay_queue_max_size: usize,
    ) -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        let incoming_payments = Arc::new(RwLock::new(HashMap::new()));

        let secondary_repayment_queue =
            SecondaryRepaymentQueue::new(repay_queue_max_size, sender.clone(), unit.clone());

        Self {
            fee_reserve,
            sender,
            receiver: Arc::new(Mutex::new(Some(receiver))),
            payment_states: Arc::new(Mutex::new(payment_states)),
            failed_payment_check: Arc::new(Mutex::new(fail_payment_check)),
            payment_delay,
            wait_invoice_cancel_token: CancellationToken::new(),
            wait_invoice_is_active: Arc::new(AtomicBool::new(false)),
            incoming_payments,
            pending_incoming_payments: Arc::new(Mutex::new(HashMap::new())),
            manual_approval_incoming: false,
            accept_voting_requests: false,
            voting_fee_sat: 1,
            voting_options: Vec::new(),
            voting_topic: "Vote".to_string(),
            vote_ledger: Arc::new(Mutex::new(VoteLedger {
                options: HashMap::new(),
                topic: "Vote".to_string(),
                created_at: unix_timestamp_now(),
            })),
            unit,
            secondary_repayment_queue,
            exchange_rate_cache: ExchangeRateCache::new(),
        }
    }

    /// Enables or disables manual approval for incoming payments.
    pub fn with_manual_approval_incoming(mut self, enabled: bool) -> Self {
        self.manual_approval_incoming = enabled;
        self
    }

    /// Enable voting mode with specified options.
    ///
    /// When enabled, the wallet accepts arbitrary melt requests matching
    /// the configured options (e.g., "RED", "BLUE").
    pub fn with_voting(mut self, options: Vec<String>, topic: Option<String>) -> Self {
        self.accept_voting_requests = true;
        self.voting_options = options
            .into_iter()
            .map(|option| option.trim().to_uppercase())
            .collect();

        let voting_topic = topic.unwrap_or_else(|| "Vote".to_string());
        self.voting_topic = voting_topic.clone();

        self.vote_ledger = Arc::new(Mutex::new(VoteLedger {
            options: HashMap::new(),
            topic: voting_topic,
            created_at: unix_timestamp_now(),
        }));

        self
    }

    /// Set the fee for voting melt requests (default: 1 sat).
    pub fn with_voting_fee(mut self, fee_sat: u64) -> Self {
        self.voting_fee_sat = fee_sat;
        self
    }

    /// Check if a request string is a valid vote option.
    fn normalized_vote_option(request: &str) -> String {
        request.trim().to_uppercase()
    }

    /// Check if a request string is a valid vote option.
    fn is_vote_option(&self, request: &str) -> bool {
        if !self.accept_voting_requests {
            return false;
        }

        let option = Self::normalized_vote_option(request);
        self.voting_options.contains(&option)
    }

    fn checking_id_for_vote(request: &str) -> PaymentIdentifier {
        let option = Self::normalized_vote_option(request);
        PaymentIdentifier::CustomId(sha256::Hash::hash(option.as_bytes()).to_string())
    }

    /// Record a vote for an option.
    async fn record_vote(&self, option: &str, amount_sat: u64) {
        let option_key = Self::normalized_vote_option(option);

        let mut ledger = self.vote_ledger.lock().await;

        let tally = ledger
            .options
            .entry(option_key.clone())
            .or_insert(VoteTally {
                option: option_key,
                total_amount: 0,
                vote_count: 0,
            });

        tally.total_amount += amount_sat;
        tally.vote_count += 1;

        tracing::info!(
            "Recorded vote: {} +{} sats (total: {}, votes: {})",
            tally.option,
            amount_sat,
            tally.total_amount,
            tally.vote_count
        );
    }

    /// Get current vote results.
    pub async fn get_vote_results(&self) -> VoteLedger {
        self.vote_ledger.lock().await.clone()
    }

    /// Get results for a specific option.
    pub async fn get_vote_tally(&self, option: &str) -> Option<VoteTally> {
        let ledger = self.vote_ledger.lock().await;
        ledger
            .options
            .get(&Self::normalized_vote_option(option))
            .cloned()
    }

    /// Reset all votes and start fresh.
    pub async fn reset_votes(&self) {
        let mut ledger = self.vote_ledger.lock().await;
        ledger.options.clear();
        ledger.created_at = unix_timestamp_now();
    }

    /// Get total votes cast across all options.
    pub async fn get_total_votes(&self) -> (u64, u64) {
        let ledger = self.vote_ledger.lock().await;
        let total_amount = ledger
            .options
            .values()
            .map(|tally| tally.total_amount)
            .sum();
        let total_count = ledger.options.values().map(|tally| tally.vote_count).sum();
        (total_amount, total_count)
    }

    /// Marks a pending incoming payment as approved and emits the payment event.
    pub async fn approve_incoming_payment(&self, payment_identifier: &PaymentIdentifier) -> bool {
        let pending_payment = {
            let mut pending = self.pending_incoming_payments.lock().await;
            pending.remove(payment_identifier)
        };

        let Some(pending_payment) = pending_payment else {
            tracing::warn!(
                "approve_incoming_payment: payment not found in pending: {:?}",
                payment_identifier
            );
            return false;
        };

        let payment_amount = pending_payment.payment_amount;

        let response = WaitPaymentResponse {
            payment_identifier: payment_identifier.clone(),
            payment_amount: payment_amount.clone(),
            payment_id: payment_identifier.to_string(),
        };

        {
            let mut incoming = self.incoming_payments.write().await;
            incoming
                .entry(payment_identifier.clone())
                .or_insert_with(Vec::new)
                .push(response.clone());
        }

        if pending_payment.is_any_amount {
            self.secondary_repayment_queue
                .enqueue_for_repayment(payment_identifier.clone())
                .await;
        }

        if let Err(e) = self.sender.send(response).await {
            tracing::error!(
                "Failed to send approval event for {:?}: {}",
                payment_identifier,
                e
            );
            return false;
        }

        tracing::info!(
            "Approved incoming payment: {:?}, amount: {:?}",
            payment_identifier,
            payment_amount
        );

        true
    }

    /// Returns all incoming payments currently waiting for manual approval.
    pub async fn get_pending_incoming_payments(
        &self,
    ) -> Vec<(PaymentIdentifier, Amount<CurrencyUnit>)> {
        let pending = self.pending_incoming_payments.lock().await;
        pending
            .iter()
            .map(|(id, payment)| (id.clone(), payment.payment_amount.clone()))
            .collect()
    }
}

/// Struct for signaling what methods should respond via invoice description
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct FakeInvoiceDescription {
    /// State to be returned from pay invoice state
    pub pay_invoice_state: MeltQuoteState,
    /// State to be returned by check payment state
    pub check_payment_state: MeltQuoteState,
    /// Should pay invoice error
    pub pay_err: bool,
    /// Should check failure
    pub check_err: bool,
}

impl Default for FakeInvoiceDescription {
    fn default() -> Self {
        Self {
            pay_invoice_state: MeltQuoteState::Paid,
            check_payment_state: MeltQuoteState::Paid,
            pay_err: false,
            check_err: false,
        }
    }
}

#[async_trait]
impl MintPayment for FakeWallet {
    type Err = payment::Error;

    #[instrument(skip_all)]
    async fn get_settings(&self) -> Result<SettingsResponse, Self::Err> {
        let mut custom = HashMap::new();
        if self.accept_voting_requests {
            custom.insert("voting".to_string(), "enabled".to_string());
            custom.insert("voting_options".to_string(), self.voting_options.join(","));
            custom.insert("voting_topic".to_string(), self.voting_topic.clone());
        }

        Ok(SettingsResponse {
            unit: self.unit.to_string(),
            bolt11: Some(payment::Bolt11Settings {
                mpp: true,
                amountless: false,
                invoice_description: true,
            }),
            bolt12: Some(payment::Bolt12Settings { amountless: false }),
            custom,
        })
    }

    #[instrument(skip_all)]
    fn is_wait_invoice_active(&self) -> bool {
        self.wait_invoice_is_active.load(Ordering::SeqCst)
    }

    #[instrument(skip_all)]
    fn cancel_wait_invoice(&self) {
        self.wait_invoice_cancel_token.cancel()
    }

    #[instrument(skip_all)]
    async fn wait_payment_event(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = Event> + Send>>, Self::Err> {
        tracing::info!("Starting stream for fake invoices");
        let receiver = self.receiver.lock().await.take().ok_or(Error::NoReceiver)?;
        let receiver_stream = ReceiverStream::new(receiver);
        Ok(Box::pin(receiver_stream.map(move |wait_response| {
            Event::PaymentReceived(wait_response)
        })))
    }

    #[instrument(skip_all)]
    async fn get_payment_quote(
        &self,
        unit: &CurrencyUnit,
        options: OutgoingPaymentOptions,
    ) -> Result<PaymentQuoteResponse, Self::Err> {
        let (amount_msat, request_lookup_id, custom_fee_override) = match options {
            OutgoingPaymentOptions::Bolt11(bolt11_options) => {
                // If we have specific amount options, use those
                let amount_msat: u64 = if let Some(melt_options) = bolt11_options.melt_options {
                    let msats = match melt_options {
                        MeltOptions::Amountless { amountless } => {
                            let amount_msat = amountless.amount_msat;

                            if let Some(invoice_amount) =
                                bolt11_options.bolt11.amount_milli_satoshis()
                            {
                                ensure_cdk!(
                                    invoice_amount == u64::from(amount_msat),
                                    Error::UnknownInvoiceAmount.into()
                                );
                            }
                            amount_msat
                        }
                        MeltOptions::Mpp { mpp } => mpp.amount,
                    };

                    u64::from(msats)
                } else {
                    // Fall back to invoice amount
                    bolt11_options
                        .bolt11
                        .amount_milli_satoshis()
                        .ok_or(Error::UnknownInvoiceAmount)?
                };
                let payment_id =
                    PaymentIdentifier::PaymentHash(*bolt11_options.bolt11.payment_hash().as_ref());
                (amount_msat, Some(payment_id), None)
            }
            OutgoingPaymentOptions::Bolt12(bolt12_options) => {
                let offer = bolt12_options.offer;

                let amount_msat: u64 = if let Some(amount) = bolt12_options.melt_options {
                    amount.amount_msat().into()
                } else {
                    // Fall back to offer amount
                    let amount = offer.amount().ok_or(Error::UnknownInvoiceAmount)?;
                    match amount {
                        lightning::offers::offer::Amount::Bitcoin { amount_msats } => amount_msats,
                        _ => return Err(Error::UnknownInvoiceAmount.into()),
                    }
                };
                (amount_msat, None, None)
            }
            OutgoingPaymentOptions::Custom(custom_options) => {
                let request = custom_options.request.as_str();

                if !self.is_vote_option(request) {
                    return Err(cdk_common::payment::Error::UnsupportedPaymentOption);
                }

                let amount_msat: u64 = custom_options
                    .melt_options
                    .ok_or(Error::UnknownInvoiceAmount)?
                    .amount_msat()
                    .into();
                ensure_cdk!(amount_msat % 1000 == 0, Error::UnknownInvoiceAmount.into());

                let fee = convert_currency_amount(
                    self.voting_fee_sat,
                    &CurrencyUnit::Sat,
                    unit,
                    &self.exchange_rate_cache,
                )
                .await?;

                (
                    amount_msat,
                    Some(Self::checking_id_for_vote(request)),
                    Some(fee),
                )
            }
        };

        let amount = convert_currency_amount(
            amount_msat,
            &CurrencyUnit::Msat,
            unit,
            &self.exchange_rate_cache,
        )
        .await?;

        let fee = if let Some(custom_fee) = custom_fee_override {
            custom_fee
        } else {
            let relative_fee_reserve =
                (self.fee_reserve.percent_fee_reserve * amount.value() as f32) as u64;

            let absolute_fee_reserve: u64 = self.fee_reserve.min_fee_reserve.into();

            Amount::new(max(relative_fee_reserve, absolute_fee_reserve), unit.clone())
        };

        Ok(PaymentQuoteResponse {
            request_lookup_id,
            amount,
            fee,
            state: MeltQuoteState::Unpaid,
        })
    }

    #[instrument(skip_all)]
    async fn make_payment(
        &self,
        unit: &CurrencyUnit,
        options: OutgoingPaymentOptions,
    ) -> Result<MakePaymentResponse, Self::Err> {
        match options {
            OutgoingPaymentOptions::Bolt11(bolt11_options) => {
                let bolt11 = bolt11_options.bolt11;
                let payment_hash = bolt11.payment_hash().to_string();

                let amount_msat: u64 = if let Some(melt_options) = bolt11_options.melt_options {
                    melt_options.amount_msat().into()
                } else {
                    bolt11
                        .amount_milli_satoshis()
                        .ok_or(Error::UnknownInvoiceAmount)?
                };

                let description = bolt11.description().to_string();

                let status: Option<FakeInvoiceDescription> =
                    serde_json::from_str(&description).ok();

                let mut payment_states = self.payment_states.lock().await;
                let payment_status = status
                    .clone()
                    .map(|s| s.pay_invoice_state)
                    .unwrap_or(MeltQuoteState::Paid);

                let checkout_going_status = status
                    .clone()
                    .map(|s| s.check_payment_state)
                    .unwrap_or(MeltQuoteState::Paid);

                let amount_spent = if checkout_going_status == MeltQuoteState::Paid {
                    Amount::new(amount_msat, CurrencyUnit::Msat)
                } else {
                    Amount::new(0, CurrencyUnit::Msat)
                };

                payment_states.insert(payment_hash.clone(), (checkout_going_status, amount_spent));

                if let Some(description) = status {
                    if description.check_err {
                        let mut fail = self.failed_payment_check.lock().await;
                        fail.insert(payment_hash.clone());
                    }

                    ensure_cdk!(!description.pay_err, Error::UnknownInvoice.into());
                }

                let total_spent = convert_currency_amount(
                    amount_msat,
                    &CurrencyUnit::Msat,
                    unit,
                    &self.exchange_rate_cache,
                )
                .await?;

                Ok(MakePaymentResponse {
                    payment_lookup_id: PaymentIdentifier::PaymentHash(
                        *bolt11.payment_hash().as_ref(),
                    ),
                    payment_proof: Some("".to_string()),
                    status: payment_status,
                    total_spent: Amount::new(total_spent.value() + 1, unit.clone()),
                })
            }
            OutgoingPaymentOptions::Bolt12(bolt12_options) => {
                let bolt12 = bolt12_options.offer;
                let payment_lookup_id = PaymentIdentifier::CustomId(Uuid::new_v4().to_string());
                let amount_msat: u64 = if let Some(amount) = bolt12_options.melt_options {
                    amount.amount_msat().into()
                } else {
                    // Fall back to offer amount
                    let amount = bolt12.amount().ok_or(Error::UnknownInvoiceAmount)?;
                    match amount {
                        lightning::offers::offer::Amount::Bitcoin { amount_msats } => amount_msats,
                        _ => return Err(Error::UnknownInvoiceAmount.into()),
                    }
                };

                let total_spent = convert_currency_amount(
                    amount_msat,
                    &CurrencyUnit::Msat,
                    unit,
                    &self.exchange_rate_cache,
                )
                .await?;

                Ok(MakePaymentResponse {
                    payment_lookup_id,
                    payment_proof: Some("".to_string()),
                    status: MeltQuoteState::Paid,
                    total_spent: Amount::new(total_spent.value() + 1, unit.clone()),
                })
            }
            OutgoingPaymentOptions::Custom(custom_options) => {
                let request = custom_options.request.as_str();

                if !self.is_vote_option(request) {
                    return Err(cdk_common::payment::Error::UnsupportedPaymentOption);
                }

                let amount_msat: u64 = custom_options
                    .melt_options
                    .ok_or(Error::UnknownInvoiceAmount)?
                    .amount_msat()
                    .into();
                ensure_cdk!(amount_msat % 1000 == 0, Error::UnknownInvoiceAmount.into());

                let amount_sat = amount_msat / 1000;
                self.record_vote(request, amount_sat).await;

                let payment_lookup_id = Self::checking_id_for_vote(request);

                let amount_in_unit = convert_currency_amount(
                    amount_msat,
                    &CurrencyUnit::Msat,
                    unit,
                    &self.exchange_rate_cache,
                )
                .await?;

                let fee_in_unit = convert_currency_amount(
                    self.voting_fee_sat,
                    &CurrencyUnit::Sat,
                    unit,
                    &self.exchange_rate_cache,
                )
                .await?;

                Ok(MakePaymentResponse {
                    payment_lookup_id,
                    payment_proof: Some("voted".to_string()),
                    status: MeltQuoteState::Paid,
                    total_spent: Amount::new(
                        amount_in_unit.value() + fee_in_unit.value(),
                        unit.clone(),
                    ),
                })
            }
        }
    }

    #[instrument(skip_all)]
    async fn create_incoming_payment_request(
        &self,
        options: IncomingPaymentOptions,
    ) -> Result<CreateIncomingPaymentResponse, Self::Err> {
        let (payment_hash, request, amount, expiry) = match options {
            IncomingPaymentOptions::Bolt12(bolt12_options) => {
                let description = bolt12_options.description.unwrap_or_default();
                let amount = bolt12_options.amount;
                let expiry = bolt12_options.unix_expiry;

                let secret_key = SecretKey::new(&mut bitcoin::secp256k1::rand::rngs::OsRng);
                let secp_ctx = Secp256k1::new();

                let offer_builder = OfferBuilder::new(secret_key.public_key(&secp_ctx))
                    .description(description.clone());

                let (offer_builder, final_amount) = match amount {
                    Some(ref amt) => {
                        let amount_msat = convert_currency_amount(
                            amt.value(),
                            amt.unit(),
                            &CurrencyUnit::Msat,
                            &self.exchange_rate_cache,
                        )
                        .await?;
                        (offer_builder.amount_msats(amount_msat.value()), amt.clone())
                    }
                    None => (offer_builder, Amount::new(0, CurrencyUnit::Sat)),
                };

                let offer = offer_builder.build().expect("Failed to build BOLT12 offer");

                (
                    PaymentIdentifier::OfferId(offer.id().to_string()),
                    offer.to_string(),
                    final_amount,
                    expiry,
                )
            }
            IncomingPaymentOptions::Bolt11(bolt11_options) => {
                let description = bolt11_options.description.unwrap_or_default();
                let amount = bolt11_options.amount;
                let expiry = bolt11_options.unix_expiry;

                let amount_msat = convert_currency_amount(
                    amount.value(),
                    amount.unit(),
                    &CurrencyUnit::Msat,
                    &self.exchange_rate_cache,
                )
                .await?;

                let invoice = create_fake_invoice(amount_msat.value(), description.clone());
                let payment_hash = invoice.payment_hash();

                (
                    PaymentIdentifier::PaymentHash(*payment_hash.as_ref()),
                    invoice.to_string(),
                    amount,
                    expiry,
                )
            }
            IncomingPaymentOptions::Custom(_) => {
                // Custom payment methods are not supported by fake wallet
                return Err(cdk_common::payment::Error::UnsupportedPaymentOption);
            }
        };

        let is_any_amount = amount.value() == 0;
        let final_amount = if is_any_amount {
            use bitcoin::secp256k1::rand::rngs::OsRng;
            use bitcoin::secp256k1::rand::Rng;
            let mut rng = OsRng;
            let random_amount: u64 = rng.gen_range(1000..=10000);
            Amount::new(random_amount, amount.unit().clone())
        } else {
            amount
        };

        if self.manual_approval_incoming {
            let mut pending = self.pending_incoming_payments.lock().await;
            pending.insert(
                payment_hash.clone(),
                PendingIncomingPayment {
                    payment_amount: final_amount,
                    is_any_amount,
                },
            );
        } else {
            let sender = self.sender.clone();
            let duration = time::Duration::from_secs(self.payment_delay);
            let payment_hash_clone = payment_hash.clone();
            let incoming_payment = self.incoming_payments.clone();

            tokio::spawn(async move {
                time::sleep(duration).await;

                let response = WaitPaymentResponse {
                    payment_identifier: payment_hash_clone.clone(),
                    payment_amount: final_amount,
                    payment_id: payment_hash_clone.to_string(),
                };
                let mut incoming = incoming_payment.write().await;
                incoming
                    .entry(payment_hash_clone.clone())
                    .or_insert_with(Vec::new)
                    .push(response.clone());

                if sender.send(response).await.is_err() {
                    tracing::error!("Failed to send label: {:?}", payment_hash_clone);
                }
            });

            if is_any_amount {
                self.secondary_repayment_queue
                    .enqueue_for_repayment(payment_hash.clone())
                    .await;
            }
        }

        Ok(CreateIncomingPaymentResponse {
            request_lookup_id: payment_hash,
            request,
            expiry,
            extra_json: None,
        })
    }

    #[instrument(skip_all)]
    async fn check_incoming_payment_status(
        &self,
        request_lookup_id: &PaymentIdentifier,
    ) -> Result<Vec<WaitPaymentResponse>, Self::Err> {
        Ok(self
            .incoming_payments
            .read()
            .await
            .get(request_lookup_id)
            .cloned()
            .unwrap_or(vec![]))
    }

    #[instrument(skip_all)]
    async fn check_outgoing_payment(
        &self,
        request_lookup_id: &PaymentIdentifier,
    ) -> Result<MakePaymentResponse, Self::Err> {
        if self.accept_voting_requests && matches!(request_lookup_id, PaymentIdentifier::CustomId(_)) {
            return Ok(MakePaymentResponse {
                payment_lookup_id: request_lookup_id.clone(),
                payment_proof: Some("voted".to_string()),
                status: MeltQuoteState::Paid,
                total_spent: Amount::new(0, CurrencyUnit::Msat),
            });
        }

        // For fake wallet if the state is not explicitly set default to paid
        let states = self.payment_states.lock().await;
        let status = states.get(&request_lookup_id.to_string()).cloned();

        let (status, total_spent) =
            status.unwrap_or((MeltQuoteState::Unknown, Amount::new(0, CurrencyUnit::Msat)));

        let fail_payments = self.failed_payment_check.lock().await;

        if fail_payments.contains(&request_lookup_id.to_string()) {
            return Err(payment::Error::InvoicePaymentPending);
        }

        Ok(MakePaymentResponse {
            payment_lookup_id: request_lookup_id.clone(),
            payment_proof: (status == MeltQuoteState::Paid).then(|| "".to_string()),
            status,
            total_spent,
        })
    }
}

/// Create fake invoice
///
/// # Panics
///
/// Panics if the hardcoded secret key or payment hash bytes are invalid.
#[instrument]
pub fn create_fake_invoice(amount_msat: u64, description: String) -> Bolt11Invoice {
    let private_key = SecretKey::from_slice(
        &[
            0xe1, 0x26, 0xf6, 0x8f, 0x7e, 0xaf, 0xcc, 0x8b, 0x74, 0xf5, 0x4d, 0x26, 0x9f, 0xe2,
            0x06, 0xbe, 0x71, 0x50, 0x00, 0xf9, 0x4d, 0xac, 0x06, 0x7d, 0x1c, 0x04, 0xa8, 0xca,
            0x3b, 0x2d, 0xb7, 0x34,
        ][..],
    )
    .expect("Valid 32-byte secret key");

    use bitcoin::secp256k1::rand::rngs::OsRng;
    use bitcoin::secp256k1::rand::Rng;
    let mut rng = OsRng;
    let mut random_bytes = [0u8; 32];
    rng.fill(&mut random_bytes);

    let payment_hash = sha256::Hash::from_slice(&random_bytes).expect("Valid 32-byte hash input");
    let payment_secret = PaymentSecret([42u8; 32]);

    InvoiceBuilder::new(Currency::Bitcoin)
        .description(description)
        .payment_hash(payment_hash)
        .payment_secret(payment_secret)
        .amount_milli_satoshis(amount_msat)
        .current_timestamp()
        .min_final_cltv_expiry_delta(144)
        .build_signed(|hash| Secp256k1::new().sign_ecdsa_recoverable(hash, &private_key))
        .expect("Failed to build fake invoice")
}

#[cfg(test)]
mod tests {
    use super::*;

    use cdk_common::nuts::MeltOptions;
    use cdk_common::payment::{
        Bolt11IncomingPaymentOptions, CustomOutgoingPaymentOptions, IncomingPaymentOptions,
        OutgoingPaymentOptions,
    };

    fn test_wallet() -> FakeWallet {
        FakeWallet::new(
            FeeReserve {
                min_fee_reserve: 1.into(),
                percent_fee_reserve: 0.0,
            },
            HashMap::new(),
            HashSet::new(),
            0,
            CurrencyUnit::Sat,
        )
    }

    fn test_wallet_with_voting() -> FakeWallet {
        test_wallet()
            .with_voting(
                vec!["RED".to_string(), "BLUE".to_string()],
                Some("Red vs Blue".to_string()),
            )
            .with_voting_fee(1)
    }

    fn vote_payment_options(option: &str, amount_msat: u64) -> OutgoingPaymentOptions {
        OutgoingPaymentOptions::Custom(Box::new(CustomOutgoingPaymentOptions {
            method: "vote".to_string(),
            request: option.to_string(),
            max_fee_amount: None,
            timeout_secs: None,
            melt_options: Some(MeltOptions::new_amountless(amount_msat)),
            extra_json: None,
        }))
    }

    #[tokio::test]
    async fn incoming_manual_approval_requires_explicit_approval() {
        let wallet = test_wallet().with_manual_approval_incoming(true);

        let created = wallet
            .create_incoming_payment_request(IncomingPaymentOptions::Bolt11(
                Bolt11IncomingPaymentOptions {
                    description: Some("manual incoming".to_string()),
                    amount: Amount::new(1234, CurrencyUnit::Sat),
                    unix_expiry: None,
                },
            ))
            .await
            .expect("incoming request should be created");

        let initial = wallet
            .check_incoming_payment_status(&created.request_lookup_id)
            .await
            .expect("status query should work");
        assert!(initial.is_empty());

        let pending = wallet.get_pending_incoming_payments().await;
        assert_eq!(pending.len(), 1);

        let approved = wallet
            .approve_incoming_payment(&created.request_lookup_id)
            .await;
        assert!(approved);

        let after_approval = wallet
            .check_incoming_payment_status(&created.request_lookup_id)
            .await
            .expect("status query should work after approval");
        assert_eq!(after_approval.len(), 1);
        assert_eq!(after_approval[0].payment_amount.value(), 1234);
    }

    #[tokio::test]
    async fn auto_mode_does_not_require_approval() {
        let wallet = test_wallet().with_manual_approval_incoming(false);

        let _created = wallet
            .create_incoming_payment_request(IncomingPaymentOptions::Bolt11(
                Bolt11IncomingPaymentOptions {
                    description: Some("auto mode".to_string()),
                    amount: Amount::new(5000, CurrencyUnit::Sat),
                    unix_expiry: None,
                },
            ))
            .await
            .expect("incoming request should be created");

        let pending = wallet.get_pending_incoming_payments().await;
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn voting_records_votes_correctly() {
        let wallet = test_wallet_with_voting();

        let red_response = wallet
            .make_payment(&CurrencyUnit::Sat, vote_payment_options("RED", 100_000))
            .await
            .expect("vote should succeed");
        assert_eq!(red_response.status, MeltQuoteState::Paid);

        let blue_response = wallet
            .make_payment(&CurrencyUnit::Sat, vote_payment_options("BLUE", 200_000))
            .await
            .expect("vote should succeed");
        assert_eq!(blue_response.status, MeltQuoteState::Paid);

        let results = wallet.get_vote_results().await;
        assert_eq!(results.options.len(), 2);
        assert_eq!(results.topic, "Red vs Blue");

        let red = results.options.get("RED").expect("RED should exist");
        assert_eq!(red.total_amount, 100);
        assert_eq!(red.vote_count, 1);

        let blue = results.options.get("BLUE").expect("BLUE should exist");
        assert_eq!(blue.total_amount, 200);
        assert_eq!(blue.vote_count, 1);

        let (total_amount, total_count) = wallet.get_total_votes().await;
        assert_eq!(total_amount, 300);
        assert_eq!(total_count, 2);
    }

    #[tokio::test]
    async fn voting_is_case_insensitive() {
        let wallet = test_wallet_with_voting();

        wallet
            .make_payment(&CurrencyUnit::Sat, vote_payment_options("red", 50_000))
            .await
            .expect("vote should succeed");

        wallet
            .make_payment(&CurrencyUnit::Sat, vote_payment_options("RED", 50_000))
            .await
            .expect("vote should succeed");

        let tally = wallet
            .get_vote_tally("RED")
            .await
            .expect("RED should exist");
        assert_eq!(tally.total_amount, 100);
        assert_eq!(tally.vote_count, 2);
    }

    #[tokio::test]
    async fn invalid_vote_option_is_rejected() {
        let wallet = test_wallet_with_voting();

        let result = wallet
            .make_payment(&CurrencyUnit::Sat, vote_payment_options("GREEN", 100_000))
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn voting_disabled_rejects_votes() {
        let wallet = test_wallet();

        let result = wallet
            .make_payment(&CurrencyUnit::Sat, vote_payment_options("RED", 100_000))
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn reset_votes_clears_vote_data() {
        let wallet = test_wallet_with_voting();

        wallet
            .make_payment(&CurrencyUnit::Sat, vote_payment_options("RED", 100_000))
            .await
            .expect("vote should succeed");

        wallet.reset_votes().await;

        let results = wallet.get_vote_results().await;
        assert!(results.options.is_empty());

        let (total_amount, total_count) = wallet.get_total_votes().await;
        assert_eq!(total_amount, 0);
        assert_eq!(total_count, 0);
    }

    #[tokio::test]
    async fn get_settings_advertises_voting_when_enabled() {
        let wallet = test_wallet_with_voting();

        let settings = wallet.get_settings().await.expect("settings should work");
        assert_eq!(settings.custom.get("voting"), Some(&"enabled".to_string()));
        assert_eq!(
            settings.custom.get("voting_options"),
            Some(&"RED,BLUE".to_string())
        );
        assert_eq!(
            settings.custom.get("voting_topic"),
            Some(&"Red vs Blue".to_string())
        );
    }

    #[tokio::test]
    async fn get_payment_quote_for_vote_requires_whole_sat_amount() {
        let wallet = test_wallet_with_voting();

        let result = wallet
            .get_payment_quote(&CurrencyUnit::Sat, vote_payment_options("RED", 100_001))
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn get_payment_quote_for_vote_rejects_sub_sat_amount() {
        let wallet = test_wallet_with_voting();

        let result = wallet
            .get_payment_quote(&CurrencyUnit::Sat, vote_payment_options("RED", 500))
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn get_payment_quote_for_vote_uses_voting_fee() {
        let wallet = test_wallet()
            .with_voting(
                vec!["RED".to_string(), "BLUE".to_string()],
                Some("Red vs Blue".to_string()),
            )
            .with_voting_fee(7);

        let quote = wallet
            .get_payment_quote(&CurrencyUnit::Sat, vote_payment_options("RED", 100_000))
            .await
            .expect("quote should succeed");

        assert_eq!(quote.amount.value(), 100);
        assert_eq!(quote.fee.value(), 7);
    }

    #[tokio::test]
    async fn check_outgoing_payment_for_vote_returns_paid() {
        let wallet = test_wallet_with_voting();
        let response = wallet
            .check_outgoing_payment(&PaymentIdentifier::CustomId("vote-lookup".to_string()))
            .await
            .expect("check should work");

        assert_eq!(response.status, MeltQuoteState::Paid);
        assert_eq!(response.payment_proof, Some("voted".to_string()));
    }

    #[tokio::test]
    async fn check_outgoing_payment_custom_without_voting_is_unknown() {
        let wallet = test_wallet();
        let response = wallet
            .check_outgoing_payment(&PaymentIdentifier::CustomId("vote-lookup".to_string()))
            .await
            .expect("check should work");

        assert_eq!(response.status, MeltQuoteState::Unknown);
        assert_eq!(response.payment_proof, None);
    }
}
