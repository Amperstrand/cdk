//! NUT-32: Cashu Futures (draft, private spike).
//!
//! Application-layer proposal: a future is ordinary Cashu ecash whose
//! `unit` follows `future:<base>-<quote>:<maturity>` and whose proof
//! secret carries exactly one NUT-10 tag `["future", "1", "<terms-uri>"]`
//! referencing a content-addressed, mint-signed terms blob.
//!
//! This module holds the pure primitives shared by the mint (validation,
//! registration) and tests: unit grammar, secret-tag validation, canonical
//! terms encoding, and the BIP-340 signing payload.

use bitcoin::hashes::{sha256, Hash};
use serde::{Deserialize, Serialize};

/// Domain prefix for the terms signing payload (NUT-32 draft).
pub const TERMS_DOMAIN: &str = "Cashu_NUT32_Terms_v1:";
/// Method name used for future issuance/redemption quotes.
pub const FUTURE_METHOD: &str = "future";
/// Amounts every future-series keyset supports (egg counts are small;
/// powers of two sum to any integer).
pub const FUTURE_KEYSET_AMOUNTS: &[u64] = &[1, 2, 4, 8, 16, 32, 64, 128, 256, 512];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FutureUnit {
    /// Lowercase ASCII `[a-z0-9]+`.
    pub base: String,
    /// Lowercase ASCII `[a-z0-9]+`.
    pub quote: String,
    /// Unix seconds of maturity (UTC).
    pub maturity: u64,
}

impl FutureUnit {
    pub fn unit_string(&self) -> String {
        format!("future:{}-{}:{}", self.base, self.quote, format_maturity(self.maturity))
    }
}

/// `yyyymmddthhmmssz` from unix seconds (UTC, lowercase separators —
/// Cashu unit identifiers are lowercase by established practice; cdk
/// normalizes custom units to lowercase NFC).
pub fn format_maturity(unix: u64) -> String {
    let date = time_from_unix(unix);
    format!(
        "{:04}{:02}{:02}t{:02}{:02}{:02}z",
        date.0, date.1, date.2, date.3, date.4, date.5
    )
}

/// Civil date-time (y, m, d, h, mi, s) from unix seconds — Howard Hinnant's
/// civil_from_days, no external time crate needed.
fn time_from_unix(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (
        y,
        m as u32,
        d as u32,
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    )
}

fn unix_from_date(y: i64, m: u32, d: u32, h: u32, mi: u32, s: u32) -> Option<u64> {
    // Days from civil (Howard Hinnant's days_from_civil).
    let yy = if m <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mm = m as i64;
    let doy = (153 * (if mm > 2 { mm - 3 } else { mm + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h as i64 * 3600 + mi as i64 * 60 + s as i64;
    u64::try_from(secs).ok()
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Parse the canonical NUT-32 unit grammar:
/// `future:<base>-<quote>:<YYYYMMDD>T<hhmmss>Z`.
///
/// Implementations MUST reject lowercase `t`/`z`, offsets, fractional
/// seconds, and impossible dates (NUT-32 draft, Unit).
pub fn parse_future_unit(unit: &str) -> Result<FutureUnit, String> {
    let rest = unit
        .strip_prefix("future:")
        .ok_or_else(|| "unit must start with `future:`".to_string())?;
    let (pair, stamp) = rest
        .rsplit_once(':')
        .ok_or_else(|| "unit must be future:<base>-<quote>:<maturity>".to_string())?;
    let (base, quote) = pair
        .split_once('-')
        .ok_or_else(|| "unit pair must be <base>-<quote>".to_string())?;
    let valid_id = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if !valid_id(base) || !valid_id(quote) {
        return Err("base and quote must be lowercase [a-z0-9]+".into());
    }
    // Timestamp: strict `yyyymmddthhmmssz`, lowercase separators only —
    // the unit string lowercases whole (cdk custom-unit normalization).
    let bytes = stamp.as_bytes();
    if bytes.len() != 16
        || bytes[8] != b't'
        || bytes[15] != b'z'
        || bytes[..8].iter().chain(&bytes[9..15]).any(|b| !b.is_ascii_digit())
    {
        return Err(format!("maturity `{stamp}` must be yyyymmddthhmmssz (UTC, lowercase, no offsets, no fractions)"));
    }
    let num = |r: std::ops::Range<usize>| -> i64 { stamp[r].parse().expect("digits checked") };
    let (y, mo, d) = (num(0..4), num(4..6), num(6..8));
    let (h, mi, s) = (num(9..11), num(11..13), num(13..15));
    let dim = match mo {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(y) => 29,
        2 => 28,
        _ => return Err(format!("impossible month {mo}")),
    };
    if !(1..=31).contains(&d) || d > dim || h > 23 || mi > 59 || s > 59 {
        return Err(format!("impossible date in `{stamp}`"));
    }
    let maturity =
        unix_from_date(y, mo as u32, d as u32, h as u32, mi as u32, s as u32).ok_or("date out of range")?;
    Ok(FutureUnit {
        base: base.to_string(),
        quote: quote.to_string(),
        maturity,
    })
}

/// True when the CurrencyUnit's string form obeys the future grammar.
pub fn is_future_unit_str(unit: &str) -> bool {
    parse_future_unit(unit).is_ok()
}

/// A validated NUT-32 future tag from a proof secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FutureTag {
    pub version: String,
    pub terms_uri: String,
}

/// Extract the digest hex from a canonical content-addressed terms URI
/// (`https://…/<64 hex>`), if the shape is right.
pub fn terms_digest_from_uri(uri: &str) -> Option<String> {
    let path = uri.strip_prefix("https://")?;
    let last = path.rsplit('/').next()?;
    if last.len() == 64 && last.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        Some(last.to_string())
    } else {
        None
    }
}

/// Validate a proof secret for a future-unit proof.
///
/// The secret must be the NUT-10 object form
/// `{"secret": "<string>", "tags": [["future", "1", "<terms-uri>"]]}`
/// with EXACTLY ONE `future` tag (version "1") whose URI is a canonical
/// content-addressed https URL (NUT-32 draft, Future secret).
pub fn validate_future_secret(secret: &str) -> Result<FutureTag, String> {
    let value: serde_json::Value =
        serde_json::from_str(secret).map_err(|e| format!("secret is not NUT-10 JSON: {e}"))?;
    let obj = value
        .as_object()
        .ok_or_else(|| "secret must be the NUT-10 object form".to_string())?;
    if !obj.contains_key("secret") || !obj["secret"].is_string() {
        return Err("NUT-10 secret object requires a string `secret` field".into());
    }
    let tags = obj
        .get("tags")
        .and_then(|t| t.as_array())
        .ok_or_else(|| "NUT-10 secret requires a `tags` array".to_string())?;
    let mut futures: Vec<FutureTag> = Vec::new();
    for tag in tags {
        let arr = tag
            .as_array()
            .ok_or_else(|| "tags entries must be arrays".to_string())?;
        let strs: Vec<&str> = arr
            .iter()
            .map(|v| v.as_str().ok_or_else(|| "tags entries must be strings".to_string()))
            .collect::<Result<_, _>>()?;
        if strs.first() == Some(&"future") {
            if strs.len() != 3 {
                return Err("future tag must be [future, version, terms-uri]".into());
            }
            futures.push(FutureTag {
                version: strs[1].to_string(),
                terms_uri: strs[2].to_string(),
            });
        }
    }
    match futures.len() {
        1 => {}
        0 => return Err("future-unit proof secret carries no `future` tag".into()),
        n => return Err(format!("future-unit proof secret carries {n} `future` tags; exactly one required")),
    }
    let tag = futures.pop().expect("checked len == 1");
    if tag.version != "1" {
        return Err(format!("unsupported future tag version {}", tag.version));
    }
    if terms_digest_from_uri(&tag.terms_uri).is_none() {
        return Err(format!("terms URI `{}` is not content-addressed https…/<sha256>", tag.terms_uri));
    }
    Ok(tag)
}

/// Canonical JSON: object keys sorted (serde_json Value backed by BTreeMap),
/// no whitespace. All terms quantities are decimal strings per the draft, so
/// no float canonicalization is needed; non-string scalars are rejected to
/// keep the encoding unambiguous.
pub fn canonical_json(value: &serde_json::Value) -> Result<String, String> {
    fn walk(v: &serde_json::Value, out: &mut String) -> Result<(), String> {
        match v {
            serde_json::Value::Object(map) => {
                // serde_json's default map is a BTreeMap: iteration is
                // already key-sorted. Refuse duplicate-key ambiguity by
                // construction (a map cannot hold duplicates).
                out.push('{');
                let mut first = true;
                for (k, val) in map {
                    if !first {
                        out.push(',');
                    }
                    first = false;
                    out.push_str(&serde_json::to_string(k).map_err(|e| e.to_string())?);
                    out.push(':');
                    walk(val, out)?;
                }
                out.push('}');
            }
            serde_json::Value::String(s) => {
                out.push_str(&serde_json::to_string(s).map_err(|e| e.to_string())?);
            }
            serde_json::Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    walk(item, out)?;
                }
                out.push(']');
            }
            serde_json::Value::Bool(_) => {
                out.push_str(&serde_json::to_string(v).map_err(|e| e.to_string())?);
            }
            serde_json::Value::Null => out.push_str("null"),
            serde_json::Value::Number(_) => {
                return Err("canonical NUT-32 terms use decimal strings, not JSON numbers".into());
            }
        }
        Ok(())
    }
    let mut out = String::new();
    walk(value, &mut out)?;
    Ok(out)
}

/// Build the exact signed payload: the domain prefix followed by the
/// canonical JSON of `{"mint": …, "terms": …}`. The signature is BIP-340
/// over the SHA-256 digest of these bytes (NUT-32 draft, Terms blob).
pub fn terms_signing_payload(mint_url: &str, terms: &serde_json::Value) -> Result<[u8; 32], String> {
    let envelope = serde_json::json!({
        "mint": mint_url,
        "terms": terms,
    });
    let canonical = canonical_json(&envelope)?;
    let mut msg = TERMS_DOMAIN.as_bytes().to_vec();
    msg.extend_from_slice(canonical.as_bytes());
    Ok(sha256::Hash::hash(&msg).to_byte_array())
}

/// SHA-256 digest of the exact blob bytes — the content address.
pub fn content_digest(bytes: &[u8]) -> String {
    let digest = sha256::Hash::hash(bytes);
    digest.to_string()
}

/// A persisted NUT-32 series registration (mint DB).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Nut32Series {
    /// Canonical unit string (`future:farm-egg:20260918T160000Z`).
    pub unit: String,
    /// sha256 hex of the exact terms blob bytes.
    pub terms_sha256: String,
    /// Keyset id serving this series (hex).
    pub keyset_id: String,
    pub registered_at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_unit() {
        let u = parse_future_unit("future:farm-egg:20260918t160000z").expect("valid");
        assert_eq!(u.base, "farm");
        assert_eq!(u.quote, "egg");
        // 2026-09-18T16:00:00Z
        assert_eq!(u.maturity, 1_789_747_200);
        assert_eq!(u.unit_string(), "future:farm-egg:20260918t160000z");
    }

    #[test]
    fn rejects_grammar_violations() {
        // uppercase t / z — the unit string lowercases whole (cdk
        // custom-unit normalization; established Cashu practice)
        assert!(parse_future_unit("future:farm-egg:20260918T160000Z").is_err());
        // offsets and fractions
        assert!(parse_future_unit("future:farm-egg:20260918t160000+01:00").is_err());
        assert!(parse_future_unit("future:farm-egg:20260918t160000.5z").is_err());
        // impossible dates
        assert!(parse_future_unit("future:farm-egg:20260230t160000z").is_err()); // no Feb 30
        assert!(parse_future_unit("future:farm-egg:20260229t160000z").is_err()); // 2026 not leap
        assert!(parse_future_unit("future:farm-egg:20240229t160000z").is_ok()); // 2024 leap
        assert!(parse_future_unit("future:farm-egg:20260918t246000z").is_err()); // hour 24
        assert!(parse_future_unit("future:farm-egg:20260918t160060z").is_err()); // second 60
        // identifier grammar
        assert!(parse_future_unit("future:Farm-egg:20260918t160000z").is_err()); // uppercase base
        assert!(parse_future_unit("future:farm_egg:20260918t160000z").is_err()); // underscore
        assert!(parse_future_unit("future:farm-:20260918t160000z").is_err()); // empty quote
        // not a future unit at all
        assert!(parse_future_unit("sat").is_err());
        assert!(parse_future_unit("eur").is_err());
    }

    #[test]
    fn maturity_roundtrip() {
        for unix in [0u64, 1_789_161_600, 4_102_444_800, 951_782_400] {
            let stamp = format_maturity(unix);
            let parsed = parse_future_unit(&format!("future:a-b:{stamp}")).expect("roundtrip");
            assert_eq!(parsed.maturity, unix, "{stamp}");
        }
    }

    #[test]
    fn validates_secret_tags() {
        let uri = "https://giftcard.cashu.exchange/terms/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let good = format!(r#"{{"secret":"deadbeef","tags":[["future","1","{uri}"]]}}"#);
        let tag = validate_future_secret(&good).expect("valid");
        assert_eq!(tag.terms_uri, uri);

        // zero tags / missing secret field
        assert!(validate_future_secret(r#"{"secret":"x"}"#).is_err());
        assert!(validate_future_secret(r#"{"tags":[["future","1","https://x/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]]}"#).is_err());
        // two future tags
        let two = format!(r#"{{"secret":"x","tags":[["future","1","{uri}"],["future","1","{uri}"]]}}"#);
        assert!(validate_future_secret(&two).is_err());
        // unknown version
        let v2 = format!(r#"{{"secret":"x","tags":[["future","2","{uri}"]]}}"#);
        assert!(validate_future_secret(&v2).is_err());
        // non content-addressed URI
        let plain = r#"{"secret":"x","tags":[["future","1","https://example.com/terms/latest"]]}"#;
        assert!(validate_future_secret(plain).is_err());
        // http (not https)
        let http = r#"{"secret":"x","tags":[["future","1","http://example.com/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]]}"#;
        assert!(validate_future_secret(http).is_err());
        // non-JSON secret (plain hex like today's deterministic secrets)
        assert!(validate_future_secret("deadbeef").is_err());
        // other tags are fine as long as exactly one future tag exists
        let mixed = format!(r#"{{"secret":"x","tags":[["purpose","test"],["future","1","{uri}"]]}}"#);
        assert!(validate_future_secret(&mixed).is_ok());
        // digest uppercase rejected (canonical lowercase hex only)
        let upper = format!(r#"{{"secret":"x","tags":[["future","1","https://x/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"]]}}"#);
        assert!(validate_future_secret(&upper).is_err());
    }

    #[test]
    fn canonical_json_sorts_keys_and_rejects_numbers() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"b":"1","a":"2"}"#).unwrap();
        assert_eq!(canonical_json(&v).unwrap(), r#"{"a":"2","b":"1"}"#);
        let nested: serde_json::Value =
            serde_json::from_str(r#"{"z":{"b":"1","a":"2"},"y":[{"k":"v"}]}"#).unwrap();
        assert_eq!(canonical_json(&nested).unwrap(), r#"{"y":[{"k":"v"}],"z":{"a":"2","b":"1"}}"#);
        let num: serde_json::Value = serde_json::from_str(r#"{"strike":1000}"#).unwrap();
        assert!(canonical_json(&num).is_err());
    }

    #[test]
    fn signing_payload_is_stable() {
        let terms: serde_json::Value = serde_json::from_str(
            r#"{"unit":"future:farm-egg:20260918t160000z","contract_size":"1"}"#,
        )
        .unwrap();
        let a = terms_signing_payload("https://m.example", &terms).unwrap();
        // Same content, different input key order → same digest.
        let terms2: serde_json::Value = serde_json::from_str(
            r#"{"contract_size":"1","unit":"future:farm-egg:20260918t160000z"}"#,
        )
        .unwrap();
        let b = terms_signing_payload("https://m.example", &terms2).unwrap();
        assert_eq!(a, b);
        // Different mint URL → different digest.
        let c = terms_signing_payload("https://other.example", &terms).unwrap();
        assert_ne!(a, c);
    }

    #[test]
    fn content_digest_matches_uri_segment() {
        let blob = b"{\"mint\":\"https://m\"}";
        let digest = content_digest(blob);
        let uri = format!("https://m.example/terms/{digest}");
        assert_eq!(terms_digest_from_uri(&uri).as_deref(), Some(digest.as_str()));
    }
}
