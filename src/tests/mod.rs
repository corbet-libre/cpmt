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
