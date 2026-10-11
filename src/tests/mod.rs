use super::*;
use ed25519_dalek::SigningKey;

fn intermediary() -> (SigningKey, VerifyingKey) {
    let sk = SigningKey::from_bytes(&[11u8; 32]);
    let vk = sk.verifying_key();
    (sk, vk)
}

fn signed(commitment: &str, valid_until: u64, id: &str) -> Entitlement {
    let (sk, _) = intermediary();
    let msg = assertion_bytes(commitment, valid_until, id);
    use ed25519_dalek::Signer;
    let sig = sk.sign(&msg).to_bytes().to_vec();
    Entitlement {
        commitment: commitment.into(),
        valid_until,
        assertion_id: id.into(),
        signature: sig,
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn roundtrip_and_expiry() {
    let (_, vk) = intermediary();
    let e = signed(&commitment("alpha", b"m1"), 2000, "a-1");
    assert!(verify(&e, &vk, 1000).is_ok());
    let debug = format!("{e:?}");
    assert!(!debug.contains(&e.commitment));
    assert!(!debug.contains(&e.assertion_id));
    assert!(!debug.contains("signature"));
    assert_eq!(verify(&e, &vk, 2000), Err(Error::Rejected));
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn provider_ids_rejected() {
    let (_, vk) = intermediary();
    for forbidden in ["cus_123", "pay_123", "sub_123"] {
        let e = signed(forbidden, 2000, "a-2");
        assert_eq!(verify(&e, &vk, 1000), Err(Error::Rejected));
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn commitment_is_community_scoped() {
    assert_ne!(commitment("alpha", b"m1"), commitment("beta", b"m1"));
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn signed_field_limits_and_forged_assertions_are_refused() {
    let (_, key) = intermediary();
    for (binding, id) in [
        (String::new(), "id".into()),
        ("x".repeat(129), "id".into()),
        ("binding".into(), String::new()),
        ("binding".into(), "x".repeat(129)),
    ] {
        assert_eq!(
            verify(&signed(&binding, 20, &id), &key, 10),
            Err(Error::Rejected)
        );
    }
    let valid = signed(&"x".repeat(128), 20, &"y".repeat(128));
    assert_eq!(verify(&valid, &key, 10), Ok(()));
    let encoded = serde_json::to_vec(&valid).unwrap();
    let decoded: Entitlement = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(verify(&decoded, &key, 10), Ok(()));
    for signature in [vec![], vec![0; 63], vec![0; 65], vec![0; 64]] {
        let mut forged = valid.clone();
        forged.signature = signature;
        assert_eq!(verify(&forged, &key, 10), Err(Error::Rejected));
    }
    let other = SigningKey::from_bytes(&[12; 32]).verifying_key();
    assert_eq!(verify(&valid, &other, 10), Err(Error::Rejected));
    let mut forged = valid.clone();
    forged.assertion_id = "another".into();
    assert_eq!(verify(&forged, &key, 10), Err(Error::Rejected));
    forged = valid.clone();
    forged.valid_until += 1;
    assert_eq!(verify(&forged, &key, 10), Err(Error::Rejected));
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn replay_and_attestation_inputs_are_domain_separated() {
    assert_eq!(replay_key("one"), replay_key("one"));
    assert_ne!(replay_key("one"), replay_key("two"));
    assert_eq!(replay_key("one").len(), 64);
    let member = commitment("alpha", b"one");
    assert_ne!(member, commitment("alpha", b"two"));
    assert_ne!(member, replay_key("one"));
    let bytes = attestation_bytes(&member, 20);
    let fields: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        fields,
        serde_json::json!(["cpmt.attestation.v1", member, 20])
    );
    assert_ne!(bytes, attestation_bytes(&member, 21));
    assert_eq!(Error::Rejected.to_string(), "entitlement rejected");
}

/// A small-order (identity) public key with the identity signature satisfies
/// the plain Ed25519 equation for every message; `verify_strict` must refuse it.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn weak_key_forgery_is_rejected() {
    use ed25519_dalek::{Signature, Verifier};
    let mut identity = [0u8; 32];
    identity[0] = 1;
    let weak = VerifyingKey::from_bytes(&identity).expect("identity point decodes");
    let mut sig = [0u8; 64];
    sig[0] = 1; // R = identity, S = 0
    let msg = assertion_bytes("binding", 2000, "a-weak");
    // The non-strict check accepts this forgery, which is why it is not used.
    assert!(weak.verify(&msg, &Signature::from_bytes(&sig)).is_ok());
    let forged = Entitlement {
        commitment: "binding".into(),
        valid_until: 2000,
        assertion_id: "a-weak".into(),
        signature: sig.to_vec(),
    };
    assert_eq!(verify(&forged, &weak, 1000), Err(Error::Rejected));
}

/// A non-canonical scalar S (S + L) is malleable; it must be refused.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn non_canonical_signature_is_rejected() {
    let (_, vk) = intermediary();
    let mut v = signed("binding", 2000, "a-nc");
    assert!(verify(&v, &vk, 1000).is_ok());
    // Add the group order L to S (little-endian) to build a second encoding.
    const L: [u8; 32] = [
        0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde,
        0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
    ];
    let mut carry = 0u16;
    for (byte, l) in v.signature[32..].iter_mut().zip(L) {
        let t = u16::from(*byte) + u16::from(l) + carry;
        *byte = (t & 0xff) as u8;
        carry = t >> 8;
    }
    assert_eq!(verify(&v, &vk, 1000), Err(Error::Rejected));
}
