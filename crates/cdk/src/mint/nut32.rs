//! NUT-32 series registry for the mint (draft spike).
//!
//! A future series is a dynamic Cashu unit (`future:<base>-<quote>:<maturity>`)
//! with its own keyset, NUT-04/05 method settings for the `future` method,
//! and an immutable content-addressed terms digest. Series are registered by
//! the operator's processor over the mintd admin route and persist in the
//! mint database; the in-memory map re-hydrates on boot.
//!
//! Enforcement points:
//! - *Issuance*: only a paid, processor-authorized quote can mint (stock
//!   cdk behavior), and outputs must use the series keyset.
//! - *Every spend* (swap and melt inputs): the revealed proof secret must
//!   carry exactly one `future` tag whose terms digest matches the series.
//!   Blinded outputs cannot be inspected at mint/swap time — that is the
//!   point of blind signatures — so secret validity is enforced where the
//!   secret is visible: at spend. A future proof with a wrong or missing
//!   tag is unspendable, hence worthless, by construction.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cdk_common::nuts::CurrencyUnit;
use cdk_common::nuts::nut04::MintMethodOptions;
use cdk_common::nuts::{MintMethodSettings, MeltMethodSettings, PaymentMethod};
use cdk_common::nuts::nut32::Nut32Series;
use cdk_common::payment::DynMintPayment;
use cdk_common::util::unix_time;
use cdk_common::Amount;
use tokio::sync::RwLock;

use crate::mint::Mint;
use crate::Error;

const NUT32_SECONDARY_NAMESPACE: &str = "nut32";
const NUT32_SERIES_KV_KEY: &str = "series";
const CDK_MINT_PRIMARY_NAMESPACE: &str = "cdk_mint";

#[derive(Default)]
pub struct Nut32State {
    enabled: AtomicBool,
    use_keyset_v2: AtomicBool,
    series: RwLock<HashMap<CurrencyUnit, Nut32Series>>,
    /// Runtime-registered processors for `(future unit, "future")` — the
    /// boot-time processors map is immutable, so series registered after
    /// boot live here and are consulted first by
    /// [`Mint::get_payment_processor`]. std RwLock: lookups happen on the
    /// synchronous processor-resolution path.
    processors: std::sync::RwLock<HashMap<(CurrencyUnit, PaymentMethod), DynMintPayment>>,
}

impl Nut32State {
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// Snapshot of the runtime-registered processors (series registered
    /// after boot); payment-check paths merge these over the boot map.
    pub fn processor_overrides(&self) -> HashMap<(CurrencyUnit, PaymentMethod), DynMintPayment> {
        self.processors
            .read()
            .expect("nut32 processors lock")
            .clone()
    }

    pub(crate) fn processor_for(
        &self,
        unit: &CurrencyUnit,
        method: &PaymentMethod,
    ) -> Option<DynMintPayment> {
        self.processors
            .read()
            .expect("nut32 processors lock")
            .get(&(unit.clone(), method.clone()))
            .cloned()
    }

    async fn series_for(&self, unit: &CurrencyUnit) -> Option<Nut32Series> {
        self.series.read().await.get(unit).cloned()
    }
}

impl Mint {
    /// The NUT-32 registry (always present; inert until enabled by mintd).
    pub fn nut32(&self) -> &Arc<Nut32State> {
        &self.nut32
    }

    /// Turn on NUT-32 enforcement and advertising for this mint. Must run
    /// before the HTTP service starts taking quotes.
    pub async fn enable_nut32(&self, use_keyset_v2: bool) -> Result<(), Error> {
        self.nut32.enabled.store(true, Ordering::SeqCst);
        self.nut32.use_keyset_v2.store(use_keyset_v2, Ordering::SeqCst);
        let mut info = self.mint_info().await?;
        if info.nuts.nut32.is_none() {
            info.nuts.nut32 = Some(cdk_common::nuts::Nut32Settings::v1());
            self.set_mint_info(info).await?;
        }
        Ok(())
    }

    /// Register (or idempotently re-affirm) a future series.
    ///
    /// Creates the unit keyset when missing, wires `backend` for
    /// `(unit, "future")`, publishes NUT-04/05 method settings so wallets
    /// and the quote path accept the unit, and persists the registration.
    pub async fn register_nut32_series(
        &self,
        unit_str: &str,
        terms_sha256: &str,
        backend: DynMintPayment,
    ) -> Result<Nut32Series, Error> {
        if !self.nut32.enabled() {
            return Err(Error::Custom(
                "NUT-32 is not enabled on this mint (CDK_MINTD_NUT32)".into(),
            ));
        }
        cdk_common::nuts::nut32::parse_future_unit(unit_str)
            .map_err(|e| Error::Custom(format!("invalid future unit `{unit_str}`: {e}")))?;
        if terms_sha256.len() != 64
            || !terms_sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(Error::Custom(
                "terms_sha256 must be a 64-char lowercase hex digest".into(),
            ));
        }
        let unit = CurrencyUnit::Custom(unit_str.into());

        if let Some(existing) = self.nut32.series_for(&unit).await {
            if existing.terms_sha256 != terms_sha256 {
                return Err(Error::Custom(format!(
                    "series {unit_str} is already registered with terms {}; terms are immutable",
                    existing.terms_sha256
                )));
            }
            return Ok(existing);
        }

        let settings = backend.get_settings().await?;
        if !settings.custom.contains_key("future") {
            return Err(Error::Custom(
                "payment processor does not operate the `future` method".into(),
            ));
        }

        let keyset_id = match self
            .keysets
            .load()
            .iter()
            .find(|k| k.active && k.unit == unit)
        {
            Some(active) => active.id,
            None => {
                let rotated = self
                    .rotate_keyset(
                        unit.clone(),
                        cdk_common::nuts::nut32::FUTURE_KEYSET_AMOUNTS.to_vec(),
                        0,
                        self.nut32.use_keyset_v2.load(Ordering::SeqCst),
                        None,
                    )
                    .await?;
                rotated.id
            }
        };

        let method = PaymentMethod::Custom("future".to_string());
        self.nut32
            .processors
            .write()
            .expect("nut32 processors lock")
            .insert((unit.clone(), method.clone()), backend);

        let mut info = self.mint_info().await?;
        let already = info
            .nuts
            .nut04
            .methods
            .iter()
            .any(|m| m.method == method && m.unit == unit);
        if !already {
            info.nuts.nut04.methods.push(MintMethodSettings {
                method: method.clone(),
                unit: unit.clone(),
                method_name: Some("Future".to_string()),
                min_amount: Some(Amount::from(1_u64)),
                max_amount: None,
                options: Some(MintMethodOptions::Custom {}),
            });
            info.nuts.nut04.disabled = false;
            info.nuts.nut05.methods.push(MeltMethodSettings {
                method,
                unit: unit.clone(),
                method_name: Some("Future".to_string()),
                min_amount: Some(Amount::from(1_u64)),
                max_amount: None,
                options: None,
            });
            info.nuts.nut05.disabled = false;
        }
        info.nuts.nut32 = Some(cdk_common::nuts::Nut32Settings::v1());
        self.set_mint_info(info).await?;

        let series = Nut32Series {
            unit: unit_str.to_string(),
            terms_sha256: terms_sha256.to_string(),
            keyset_id: keyset_id.to_string(),
            registered_at: unix_time(),
        };
        let mut map = self.nut32.series.write().await;
        map.insert(unit, series.clone());
        drop(map);
        self.persist_nut32_series().await?;
        tracing::info!(
            "NUT-32 series {unit_str} registered (keyset {}, terms {})",
            series.keyset_id,
            series.terms_sha256
        );
        Ok(series)
    }

    /// All registered series.
    pub async fn nut32_series(&self) -> Vec<Nut32Series> {
        self.nut32
            .series
            .read()
            .await
            .values()
            .cloned()
            .collect()
    }

    /// Re-register persisted series on boot (the processor map is not
    /// persisted). Idempotent with [`Mint::register_nut32_series`].
    pub async fn load_nut32_series(&self, backend: DynMintPayment) -> Result<(), Error> {
        if !self.nut32.enabled() {
            return Ok(());
        }
        let Some(bytes) = self
            .localstore
            .kv_read(
                CDK_MINT_PRIMARY_NAMESPACE,
                NUT32_SECONDARY_NAMESPACE,
                NUT32_SERIES_KV_KEY,
            )
            .await?
        else {
            return Ok(());
        };
        let list: Vec<Nut32Series> = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Custom(format!("nut32 series store corrupt: {e}")))?;
        for series in list {
            if let Err(e) = self
                .register_nut32_series(&series.unit, &series.terms_sha256, backend.clone())
                .await
            {
                tracing::error!("could not re-register NUT-32 series {}: {e}", series.unit);
            }
        }
        Ok(())
    }

    async fn persist_nut32_series(&self) -> Result<(), Error> {
        let list: Vec<Nut32Series> = self.nut32.series.read().await.values().cloned().collect();
        let bytes = serde_json::to_vec(&list)?;
        let mut tx = self.localstore.begin_transaction().await?;
        tx.kv_write(
            CDK_MINT_PRIMARY_NAMESPACE,
            NUT32_SECONDARY_NAMESPACE,
            NUT32_SERIES_KV_KEY,
            &bytes,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Spend-time validation for a future-unit input set (called from
    /// [`Mint::verify_inputs`]): every revealed secret must carry exactly
    /// one `future` tag whose URI digest equals the series' registered
    /// terms digest.
    pub(crate) async fn verify_nut32_inputs(
        &self,
        unit: &CurrencyUnit,
        inputs: &cdk_common::Proofs,
    ) -> Result<(), Error> {
        if !self.nut32.enabled() {
            return Ok(());
        }
        let Some(series) = self.nut32.series_for(unit).await else {
            return Ok(());
        };
        for proof in inputs {
            let secret = proof.secret.to_string();
            let tag = cdk_common::nuts::nut32::validate_future_secret(&secret).map_err(
                |e| {
                    tracing::warn!(
                        "rejecting future proof in keyset {}: {e}",
                        series.keyset_id
                    );
                    Error::Custom(format!("NUT-32 proof rejected: {e}"))
                },
            )?;
            let digest = cdk_common::nuts::nut32::terms_digest_from_uri(&tag.terms_uri)
                .ok_or_else(|| {
                    Error::Custom("NUT-32 terms URI is not content-addressed".to_string())
                })?;
            if digest != series.terms_sha256 {
                return Err(Error::Custom(format!(
                    "NUT-32 terms digest mismatch: proof references {digest}, series {} is {}",
                    series.unit, series.terms_sha256
                )));
            }
        }
        Ok(())
    }
}
