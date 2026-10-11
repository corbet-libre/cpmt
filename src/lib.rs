//! Payment execution through adapters, without custody or commercial policy.
//!
//! [`payments`] supplies the common contract used by fiat and cryptocurrency
//! adapters. Provider evidence stays inside the payment boundary. The signed
//! entitlement helpers below support providers that already issue assertions.
#![forbid(unsafe_code)]

pub mod access;
pub mod cost;
pub mod details;
pub mod payments;
#[cfg(feature = "providers")]
pub mod providers;
pub mod run;
pub mod simulation;
#[cfg(feature = "stripe")]
pub mod stripe;

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

/// Signed entitlement asserted by a configured provider.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Entitlement {
    /// Opaque member commitment (e.g. hash of member id + community).
    /// Must not be a provider customer id the operator could resolve.
    pub commitment: String,
    /// Expiry as unix seconds per host clock. The issuer supplies this bound.
    pub valid_until: u64,
    /// Unique assertion id for replay protection within the validity window.
    pub assertion_id: String,
    /// Intermediary signature over assertion bytes.
    pub signature: Vec<u8>,
}

impl std::fmt::Debug for Entitlement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entitlement").finish_non_exhaustive()
    }
}

/// Errors carry no customer, payment or member identity.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    /// Expired, bad signature, replayed or malformed. Uniform.
    #[error("entitlement rejected")]
    Rejected,
}

/// Canonical assertion bytes the intermediary signs.
#[must_use]
pub fn assertion_bytes(commitment: &str, valid_until: u64, assertion_id: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!([
        "cpmt.assertion.v1",
        commitment,
        valid_until,
        assertion_id,
    ]))
    .expect("JSON serializes")
}

/// Member commitment: hash of member id scoped to community.
/// The intermediary receives this commitment, never the raw member id graph.
#[must_use]
pub fn commitment(community_id: &str, member_id: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b"cpmt.commitment.v1\n");
    h.update(community_id.as_bytes());
    h.update(b"\n");
    h.update(member_id);
    hex(&h.finalize())
}

/// Verify an entitlement: signature, expiry and non-empty fields.
/// Replay-window enforcement (assertion_id seen-store with TTL) belongs
/// in the host; call [`replay_key`] for the store key.
pub fn verify(
    entitlement: &Entitlement,
    intermediary_key: &VerifyingKey,
    now_secs: u64,
) -> Result<(), Error> {
    if entitlement.commitment.is_empty()
        || entitlement.commitment.len() > 128
        || entitlement.assertion_id.is_empty()
        || entitlement.assertion_id.len() > 128
    {
        return Err(Error::Rejected);
    }
    if entitlement.commitment.starts_with("cus_")
        || entitlement.commitment.starts_with("pay_")
        || entitlement.commitment.starts_with("sub_")
    {
        // Provider customer/payment/subscription ids are forbidden here.
        return Err(Error::Rejected);
    }
    if now_secs >= entitlement.valid_until {
        return Err(Error::Rejected);
    }
    let msg = assertion_bytes(
        &entitlement.commitment,
        entitlement.valid_until,
        &entitlement.assertion_id,
    );
    let sig = Signature::from_slice(&entitlement.signature).map_err(|_| Error::Rejected)?;
    intermediary_key
        .verify_strict(&msg, &sig)
        .map_err(|_| Error::Rejected)?;
    Ok(())
}

/// Store key for host replay window: hash of assertion id.
#[must_use]
pub fn replay_key(assertion_id: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"cpmt.replay.v1\n");
    h.update(assertion_id.as_bytes());
    hex(&h.finalize())
}

/// Attestation bytes the host signs with Ed25519 for cvld issuance.
/// Contains commitment and expiry only.
#[must_use]
pub fn attestation_bytes(commitment: &str, valid_until: u64) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!([
        "cpmt.attestation.v1",
        commitment,
        valid_until
    ]))
    .expect("JSON serializes")
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "tests/support.rs"]
mod test_support;
#[cfg(test)]
mod tests;
