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

#[derive(Debug, Clone, Serialize, Deserialize)]
/// In-memory state for custom non-BOLT melt requests.
pub struct ArbitraryPayment {
    /// Original custom request payload.
    pub request: String,
    /// Parsed logical identifier extracted from the request payload.
    pub identifier: String,
    /// Requested amount in sats.
    pub amount: u64,
    /// Current payment state for the melt request.
    pub status: MeltQuoteState,
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
    manually_approved_incoming: Arc<Mutex<HashSet<PaymentIdentifier>>>,
    pending_outgoing_approvals: Arc<Mutex<HashMap<String, Amount<CurrencyUnit>>>>,
    arbitrary_payments: Arc<Mutex<HashMap<String, ArbitraryPayment>>>,
    manual_approval_incoming: bool,
    manual_approval_outgoing: bool,
    accept_arbitrary_melt_requests: bool,
    arbitrary_melt_fee_sat: u64,
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
            manually_approved_incoming: Arc::new(Mutex::new(HashSet::new())),
            pending_outgoing_approvals: Arc::new(Mutex::new(HashMap::new())),
            arbitrary_payments: Arc::new(Mutex::new(HashMap::new())),
            manual_approval_incoming: false,
            manual_approval_outgoing: false,
            accept_arbitrary_melt_requests: false,
            arbitrary_melt_fee_sat: 1,
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

    /// Enables or disables manual approval for outgoing payments.
    pub fn with_manual_approval_outgoing(mut self, enabled: bool) -> Self {
        self.manual_approval_outgoing = enabled;
        self
    }

    /// Enables or disables support for arbitrary custom melt requests.
    pub fn with_accept_arbitrary_melt_requests(mut self, enabled: bool) -> Self {
        self.accept_arbitrary_melt_requests = enabled;
        self
    }

    /// Sets additional fee in sats charged for arbitrary custom melts.
    pub fn with_arbitrary_melt_fee_sat(mut self, fee_sat: u64) -> Self {
        self.arbitrary_melt_fee_sat = fee_sat;
        self
    }

    fn is_bolt11(request: &str) -> bool {
        let lowered = request.to_lowercase();
        lowered.starts_with("lnbc") || lowered.starts_with("lntb") || lowered.starts_with("lnbcrt")
    }

    fn parse_arbitrary_request(request: &str) -> (String, u64) {
        if let Some(pos) = request.rfind(":AMOUNT:") {
            let identifier = request[..pos].to_string();
            let amount_str = &request[pos + 8..];

            if let Ok(amount) = amount_str.parse::<u64>() {
                return (identifier, amount);
            }
        }

        (request.to_string(), 0)
    }

    fn checking_id_for_arbitrary(request: &str) -> String {
        sha256::Hash::hash(request.as_bytes()).to_string()
    }

    /// Marks a pending incoming payment as approved and emits the payment event.
    pub async fn approve_incoming_payment(&self, payment_identifier: &PaymentIdentifier) -> bool {
        let pending_payment = {
            let mut pending = self.pending_incoming_payments.lock().await;
            pending.remove(payment_identifier)
        };

        let Some(pending_payment) = pending_payment else {
            return false;
        };

        {
            let mut approved = self.manually_approved_incoming.lock().await;
            approved.insert(payment_identifier.clone());
        }

        let response = WaitPaymentResponse {
            payment_identifier: payment_identifier.clone(),
            payment_amount: pending_payment.payment_amount,
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

        self.sender.send(response).await.is_ok()
    }

    /// Returns all incoming payments currently waiting for manual approval.
    pub async fn get_pending_incoming_payments(&self) -> Vec<PaymentIdentifier> {
        let pending = self.pending_incoming_payments.lock().await;
        pending.keys().cloned().collect()
    }

    /// Marks a pending outgoing payment as paid.
    pub async fn approve_outgoing_payment(&self, payment_identifier: &PaymentIdentifier) -> bool {
        let payment_key = payment_identifier.to_string();

        let approved_amount = {
            let mut pending = self.pending_outgoing_approvals.lock().await;
            pending.remove(&payment_key)
        };

        let Some(approved_amount) = approved_amount else {
            return false;
        };

        let mut states = self.payment_states.lock().await;
        states.insert(payment_key, (MeltQuoteState::Paid, approved_amount));
        true
    }

    /// Returns custom arbitrary melt requests that are still unpaid.
    pub async fn get_pending_arbitrary_payments(&self) -> Vec<(String, ArbitraryPayment)> {
        let payments = self.arbitrary_payments.lock().await;
        payments
            .iter()
            .filter(|(_, payment)| payment.status == MeltQuoteState::Unpaid)
            .map(|(checking_id, payment)| (checking_id.to_string(), payment.clone()))
            .collect()
    }

    /// Marks an arbitrary melt request as paid by checking id.
    pub async fn approve_arbitrary_payment(&self, checking_id: &str) -> bool {
        let mut payments = self.arbitrary_payments.lock().await;

        if let Some(payment) = payments.get_mut(checking_id) {
            payment.status = MeltQuoteState::Paid;
            true
        } else {
            false
        }
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
        let custom = if self.accept_arbitrary_melt_requests {
            HashMap::from([("arbitrary".to_string(), "enabled".to_string())])
        } else {
            HashMap::new()
        };

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
        let (amount_msat, request_lookup_id, arbitrary_fee_sat) = match options {
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
                (amount_msat, Some(payment_id), 0)
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
                (amount_msat, None, 0)
            }
            OutgoingPaymentOptions::Custom(custom_options) => {
                if !self.accept_arbitrary_melt_requests {
                    return Err(cdk_common::payment::Error::UnsupportedPaymentOption);
                }

                let request_str = custom_options.request.as_str();
                ensure_cdk!(
                    !Self::is_bolt11(request_str),
                    cdk_common::payment::Error::UnsupportedPaymentOption
                );

                let (identifier, parsed_amount_sat) = Self::parse_arbitrary_request(request_str);
                let amount_msat: u64 = if let Some(melt_options) = custom_options.melt_options {
                    melt_options.amount_msat().into()
                } else {
                    ensure_cdk!(parsed_amount_sat > 0, Error::UnknownInvoiceAmount.into());
                    parsed_amount_sat
                        .checked_mul(1000)
                        .ok_or(Error::UnknownInvoiceAmount)?
                };

                ensure_cdk!(amount_msat % 1000 == 0, Error::UnknownInvoiceAmount.into());

                let checking_id = Self::checking_id_for_arbitrary(request_str);
                let amount_sat = amount_msat / 1000;

                let mut payments = self.arbitrary_payments.lock().await;
                payments
                    .entry(checking_id.clone())
                    .or_insert(ArbitraryPayment {
                        request: request_str.to_string(),
                        identifier,
                        amount: amount_sat,
                        status: MeltQuoteState::Unpaid,
                    });

                (
                    amount_msat,
                    Some(PaymentIdentifier::CustomId(checking_id)),
                    self.arbitrary_melt_fee_sat,
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

        let relative_fee_reserve =
            (self.fee_reserve.percent_fee_reserve * amount.value() as f32) as u64;

        let absolute_fee_reserve: u64 = self.fee_reserve.min_fee_reserve.into();

        let mut fee = max(relative_fee_reserve, absolute_fee_reserve);

        if arbitrary_fee_sat > 0 {
            let arbitrary_fee = convert_currency_amount(
                arbitrary_fee_sat,
                &CurrencyUnit::Sat,
                unit,
                &self.exchange_rate_cache,
            )
            .await?;
            fee += arbitrary_fee.value();
        }

        Ok(PaymentQuoteResponse {
            request_lookup_id,
            amount,
            fee: Amount::new(fee, unit.clone()),
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

                if self.manual_approval_outgoing {
                    {
                        let mut payment_states = self.payment_states.lock().await;
                        payment_states.insert(
                            payment_hash.clone(),
                            (MeltQuoteState::Unpaid, Amount::new(0, CurrencyUnit::Msat)),
                        );
                    }

                    {
                        let mut pending = self.pending_outgoing_approvals.lock().await;
                        pending.insert(payment_hash, Amount::new(amount_msat, CurrencyUnit::Msat));
                    }

                    return Ok(MakePaymentResponse {
                        payment_lookup_id: PaymentIdentifier::PaymentHash(
                            *bolt11.payment_hash().as_ref(),
                        ),
                        payment_proof: None,
                        status: MeltQuoteState::Unpaid,
                        total_spent: Amount::new(0, unit.clone()),
                    });
                }

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

                if self.manual_approval_outgoing {
                    {
                        let mut payment_states = self.payment_states.lock().await;
                        payment_states.insert(
                            payment_lookup_id.to_string(),
                            (MeltQuoteState::Unpaid, Amount::new(0, CurrencyUnit::Msat)),
                        );
                    }

                    {
                        let mut pending = self.pending_outgoing_approvals.lock().await;
                        pending.insert(
                            payment_lookup_id.to_string(),
                            Amount::new(amount_msat, CurrencyUnit::Msat),
                        );
                    }

                    return Ok(MakePaymentResponse {
                        payment_lookup_id,
                        payment_proof: None,
                        status: MeltQuoteState::Unpaid,
                        total_spent: Amount::new(0, unit.clone()),
                    });
                }

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
                if !self.accept_arbitrary_melt_requests {
                    return Err(cdk_common::payment::Error::UnsupportedPaymentOption);
                }

                let request = custom_options.request;
                ensure_cdk!(
                    !Self::is_bolt11(request.as_str()),
                    cdk_common::payment::Error::UnsupportedPaymentOption
                );

                let checking_id = Self::checking_id_for_arbitrary(request.as_str());

                let (status, amount_sat) = {
                    let mut payments = self.arbitrary_payments.lock().await;

                    let amount_sat_from_request = Self::parse_arbitrary_request(request.as_str()).1;
                    let amount_sat = if let Some(melt_options) = custom_options.melt_options {
                        let amount_msat: u64 = melt_options.amount_msat().into();
                        ensure_cdk!(amount_msat % 1000 == 0, Error::UnknownInvoiceAmount.into());
                        amount_msat / 1000
                    } else {
                        amount_sat_from_request
                    };

                    ensure_cdk!(amount_sat > 0, Error::UnknownInvoiceAmount.into());

                    let payment = payments
                        .entry(checking_id.clone())
                        .or_insert(ArbitraryPayment {
                            request: request.clone(),
                            identifier: Self::parse_arbitrary_request(request.as_str()).0,
                            amount: amount_sat,
                            status: MeltQuoteState::Unpaid,
                        });

                    payment.amount = amount_sat;

                    if !self.manual_approval_outgoing {
                        payment.status = MeltQuoteState::Paid;
                    }

                    (payment.status, payment.amount)
                };

                let amount_msat = amount_sat
                    .checked_mul(1000)
                    .ok_or(Error::UnknownInvoiceAmount)?;

                let amount_in_unit = convert_currency_amount(
                    amount_msat,
                    &CurrencyUnit::Msat,
                    unit,
                    &self.exchange_rate_cache,
                )
                .await?;

                let fee_in_unit = convert_currency_amount(
                    self.arbitrary_melt_fee_sat,
                    &CurrencyUnit::Sat,
                    unit,
                    &self.exchange_rate_cache,
                )
                .await?;

                let total_spent = if status == MeltQuoteState::Paid {
                    Amount::new(amount_in_unit.value() + fee_in_unit.value(), unit.clone())
                } else {
                    Amount::new(0, unit.clone())
                };

                Ok(MakePaymentResponse {
                    payment_lookup_id: PaymentIdentifier::CustomId(checking_id),
                    payment_proof: (status == MeltQuoteState::Paid).then(|| "".to_string()),
                    status,
                    total_spent,
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
        if let PaymentIdentifier::CustomId(checking_id) = request_lookup_id {
            let payments = self.arbitrary_payments.lock().await;

            if let Some(payment) = payments.get(checking_id) {
                let total_spent = if payment.status == MeltQuoteState::Paid {
                    payment
                        .amount
                        .checked_add(self.arbitrary_melt_fee_sat)
                        .and_then(|value| value.checked_mul(1000))
                        .ok_or(Error::UnknownInvoiceAmount)?
                } else {
                    0
                };

                return Ok(MakePaymentResponse {
                    payment_lookup_id: request_lookup_id.clone(),
                    payment_proof: (payment.status == MeltQuoteState::Paid).then(|| "".to_string()),
                    status: payment.status,
                    total_spent: Amount::new(total_spent, CurrencyUnit::Msat),
                });
            }
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

    use cdk_common::payment::{
        Bolt11IncomingPaymentOptions, Bolt11OutgoingPaymentOptions, CustomOutgoingPaymentOptions,
        IncomingPaymentOptions, OutgoingPaymentOptions,
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

        let approved = wallet
            .approve_incoming_payment(&created.request_lookup_id)
            .await;
        assert!(approved);

        let after_approval = wallet
            .check_incoming_payment_status(&created.request_lookup_id)
            .await
            .expect("status query should work after approval");
        assert_eq!(after_approval.len(), 1);
    }

    #[tokio::test]
    async fn outgoing_manual_approval_keeps_payment_unpaid_until_approved() {
        let wallet = test_wallet().with_manual_approval_outgoing(true);
        let invoice = create_fake_invoice(5_000, "manual outgoing".to_string());

        let payment_response = wallet
            .make_payment(
                &CurrencyUnit::Sat,
                OutgoingPaymentOptions::Bolt11(Box::new(Bolt11OutgoingPaymentOptions {
                    bolt11: invoice,
                    max_fee_amount: None,
                    timeout_secs: None,
                    melt_options: None,
                })),
            )
            .await
            .expect("manual outgoing payment should be created");

        assert_eq!(payment_response.status, MeltQuoteState::Unpaid);

        let status_before = wallet
            .check_outgoing_payment(&payment_response.payment_lookup_id)
            .await
            .expect("check outgoing should work");
        assert_eq!(status_before.status, MeltQuoteState::Unpaid);

        let approved = wallet
            .approve_outgoing_payment(&payment_response.payment_lookup_id)
            .await;
        assert!(approved);

        let status_after = wallet
            .check_outgoing_payment(&payment_response.payment_lookup_id)
            .await
            .expect("check outgoing should work after approval");
        assert_eq!(status_after.status, MeltQuoteState::Paid);
        assert!(status_after.total_spent.value() > 0);
    }

    #[tokio::test]
    async fn arbitrary_request_flow_supports_pending_and_manual_approval() {
        let wallet = test_wallet()
            .with_accept_arbitrary_melt_requests(true)
            .with_manual_approval_outgoing(true)
            .with_arbitrary_melt_fee_sat(1);

        let custom_options = CustomOutgoingPaymentOptions {
            method: "vote".to_string(),
            request: "Red:AMOUNT:2".to_string(),
            max_fee_amount: None,
            timeout_secs: None,
            melt_options: None,
            extra_json: None,
        };

        let quote = wallet
            .get_payment_quote(
                &CurrencyUnit::Sat,
                OutgoingPaymentOptions::Custom(Box::new(custom_options.clone())),
            )
            .await
            .expect("arbitrary quote should be created");

        let expected_checking_id = FakeWallet::checking_id_for_arbitrary("Red:AMOUNT:2");
        let quote_lookup_id = quote
            .request_lookup_id
            .clone()
            .expect("quote should include lookup id");
        assert_eq!(quote_lookup_id.to_string(), expected_checking_id);

        let pay_response = wallet
            .make_payment(
                &CurrencyUnit::Sat,
                OutgoingPaymentOptions::Custom(Box::new(custom_options)),
            )
            .await
            .expect("arbitrary payment should be created");
        assert_eq!(pay_response.status, MeltQuoteState::Unpaid);

        let pending = wallet.get_pending_arbitrary_payments().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, expected_checking_id);

        let approved = wallet
            .approve_arbitrary_payment(expected_checking_id.as_str())
            .await;
        assert!(approved);

        let checked = wallet
            .check_outgoing_payment(&PaymentIdentifier::CustomId(expected_checking_id))
            .await
            .expect("arbitrary payment status should be queryable");
        assert_eq!(checked.status, MeltQuoteState::Paid);
        assert!(checked.total_spent.value() > 0);
    }
}
