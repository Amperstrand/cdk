//! mintd-side NUT-32 (Cashu Futures, draft spike) wiring.
//!
//! Gated on `CDK_MINTD_NUT32=true`. Adds token-guarded admin routes to the
//! mint's HTTP service so the operator's processor can register future
//! series and request terms signatures with the mint identity key:
//!
//! - `GET  /nut32/admin/healthz` — registration-loop liveness probe
//! - `GET  /nut32/admin/series` — registered series
//! - `POST /nut32/admin/series` — `{unit, terms_sha256}` (idempotent;
//!   terms are immutable per unit)
//! - `POST /nut32/admin/sign-terms` — `{mint, terms}` → BIP-340 signature
//!   over the `Cashu_NUT32_Terms_v1:` canonical payload with the key
//!   behind the NUT-06 `pubkey`. The key never leaves this process.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use bitcoin::hashes::Hash;
use bitcoin::secp256k1::{Keypair, Message, Secp256k1, XOnlyPublicKey};
use bip39::Mnemonic;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use cdk::nuts::CurrencyUnit;
use cdk::nuts::PaymentMethod;
use cdk::Mint;

use crate::config::Settings;

const ENV_NUT32: &str = "CDK_MINTD_NUT32";
const ENV_NUT32_ADMIN_TOKEN: &str = "CDK_MINTD_NUT32_ADMIN_TOKEN";

pub fn enabled() -> bool {
    std::env::var(ENV_NUT32)
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
}

fn admin_token() -> Option<String> {
    std::env::var(ENV_NUT32_ADMIN_TOKEN)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Derive the mint identity keypair exactly as `DbSignatory` does
/// (`Xpriv::new_master` over the seed), so signatures verify against the
/// NUT-06 `pubkey` the mint already advertises.
fn signing_keypair(settings: &Settings) -> Option<Keypair> {
    let ctx = Secp256k1::new();
    if let Some(seed) = settings.info.seed.as_ref().filter(|s| !s.is_empty()) {
        let xpriv = bitcoin::bip32::Xpriv::new_master(bitcoin::Network::Bitcoin, seed.as_bytes()).ok()?;
        return Some(xpriv.to_keypair(&ctx));
    }
    let mnemonic = settings.info.mnemonic.as_ref().filter(|m| !m.is_empty())?;
    let mnemonic = Mnemonic::from_str(mnemonic).ok()?;
    let seed = mnemonic.to_seed_normalized("");
    let xpriv = bitcoin::bip32::Xpriv::new_master(bitcoin::Network::Bitcoin, &seed).ok()?;
    Some(xpriv.to_keypair(&ctx))
}

/// Enable the registry, advertise the capability, and re-register any
/// persisted series. Runs after the boot reconcile so nothing overwrites
/// the advert afterwards.
pub async fn bootstrap(mint: &Arc<Mint>, settings: &Settings) {
    if !enabled() {
        return;
    }
    if admin_token().is_none() {
        tracing::error!(
            "{ENV_NUT32} is on but {ENV_NUT32_ADMIN_TOKEN} is not set; \
             admin routes stay disabled (404) and series cannot register"
        );
    }
    let use_keyset_v2 = settings.info.use_keyset_v2.unwrap_or(false);
    if let Err(e) = mint.enable_nut32(use_keyset_v2).await {
        tracing::error!("could not enable NUT-32: {e}");
        return;
    }
    let base_unit = settings
        .payment_backend
        .first()
        .map(|b| b.unit.clone())
        .unwrap_or(CurrencyUnit::Custom("farm".into()));
    match mint.get_payment_processor(base_unit, PaymentMethod::Custom("future".to_string())) {
        Ok(backend) => {
            if let Err(e) = mint.load_nut32_series(backend).await {
                tracing::error!("NUT-32 series re-registration failed: {e}");
            }
        }
        Err(e) => {
            tracing::warn!(
                "NUT-32 enabled but no `future` payment processor answered yet \
                 (is the processor up and farm-enabled?); persisted series will not \
                 re-register this boot: {e}"
            );
        }
    }
}

#[derive(Clone)]
struct Nut32AdminState {
    mint: Arc<Mint>,
    base_unit: CurrencyUnit,
    keypair: Option<Keypair>,
    token: String,
}

pub fn router(
    mint: Arc<Mint>,
    settings: &Settings,
) -> Router {
    let state = Nut32AdminState {
        mint,
        base_unit: settings
            .payment_backend
            .first()
            .map(|b| b.unit.clone())
            .unwrap_or(CurrencyUnit::Custom("farm".into())),
        keypair: signing_keypair(settings),
        token: admin_token().unwrap_or_default(),
    };
    Router::new()
        .route("/nut32/admin/healthz", get(healthz))
        .route("/nut32/admin/series", get(list_series).post(register_series))
        .route("/nut32/admin/sign-terms", post(sign_terms))
        .with_state(state)
}

fn authorized(state: &Nut32AdminState, headers: &HeaderMap) -> bool {
    match headers.get("x-nut32-token").and_then(|t| t.to_str().ok()) {
        Some(provided) => !state.token.is_empty() && provided == state.token,
        None => false,
    }
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, "bad or missing x-nut32-token").into_response()
}

async fn healthz(State(state): State<Nut32AdminState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let count = state.mint.nut32_series().await.len();
    (
        StatusCode::OK,
        axum::Json(serde_json::json!({ "ok": true, "series": count })),
    )
        .into_response()
}

async fn list_series(State(state): State<Nut32AdminState>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let series = state.mint.nut32_series().await;
    (StatusCode::OK, axum::Json(serde_json::json!({ "series": series }))).into_response()
}

#[derive(Deserialize)]
struct RegisterSeriesRequest {
    unit: String,
    terms_sha256: String,
}

async fn register_series(
    State(state): State<Nut32AdminState>,
    headers: HeaderMap,
    axum::Json(req): axum::Json<RegisterSeriesRequest>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let backend = match state.mint.get_payment_processor(
        state.base_unit.clone(),
        PaymentMethod::Custom("future".to_string()),
    ) {
        Ok(backend) => backend,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("no `future` payment processor available: {e}"),
            )
                .into_response()
        }
    };
    match state
        .mint
        .register_nut32_series(&req.unit, &req.terms_sha256, backend)
        .await
    {
        Ok(series) => (StatusCode::OK, axum::Json(serde_json::json!({
            "unit": series.unit,
            "terms_sha256": series.terms_sha256,
            "keyset_id": series.keyset_id,
        })))
        .into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e}")).into_response(),
    }
}

#[derive(Deserialize)]
struct SignTermsRequest {
    mint: String,
    terms: serde_json::Value,
}

#[derive(Serialize)]
struct SignTermsResponse {
    /// BIP-340 signature (hex) over SHA-256 of the domain-prefixed
    /// canonical payload.
    signature: String,
    /// The mint's NUT-06 identity pubkey, compressed hex.
    pubkey: String,
    /// sha256 hex of the exact canonical payload bytes that were signed.
    payload_sha256: String,
    /// The x-only (BIP-340) form of the pubkey, for schnorr verifiers.
    xonly: String,
}

async fn sign_terms(
    State(state): State<Nut32AdminState>,
    headers: HeaderMap,
    axum::Json(req): axum::Json<SignTermsRequest>,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let Some(keypair) = state.keypair else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "no local signing key (remote signatory configs cannot sign terms)",
        )
            .into_response();
    };
    let digest = match cdk_common::nuts::nut32::terms_signing_payload(&req.mint, &req.terms) {
        Ok(digest) => digest,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("cannot canonicalize terms: {e}")).into_response(),
    };
    let ctx = Secp256k1::new();
    let signature = ctx.sign_schnorr_no_aux_rand(&Message::from_digest(digest), &keypair);
    let payload_sha256 = bitcoin::hashes::sha256::Hash::hash(
        &[
            cdk_common::nuts::nut32::TERMS_DOMAIN.as_bytes(),
            canonical_payload_bytes(&req.mint, &req.terms).as_bytes(),
        ]
        .concat(),
    )
    .to_string();
    let xonly = XOnlyPublicKey::from_keypair(&keypair).0;
    (
        StatusCode::OK,
        axum::Json(SignTermsResponse {
            signature: signature.to_string(),
            pubkey: keypair.public_key().to_string(),
            payload_sha256,
            xonly: xonly.to_string(),
        }),
    )
        .into_response()
}

fn canonical_payload_bytes(mint_url: &str, terms: &serde_json::Value) -> String {
    let envelope = serde_json::json!({ "mint": mint_url, "terms": terms });
    cdk_common::nuts::nut32::canonical_json(&envelope).unwrap_or_default()
}
