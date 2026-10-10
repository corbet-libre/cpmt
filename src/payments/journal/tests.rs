use super::*;

#[test]
fn account_identity_binds_exact_nonsecret_configuration_with_the_existing_key_and_wipes_copies() {
    let binding =
        |scope: &str, provider: &str, account: &str, endpoint: &str, mode: Mode, key: u8| {
            let mut copy = [key; 32];
            let result = account_identity(&mut copy, scope, provider, account, endpoint, mode);
            assert_eq!(copy, [0; 32]);
            result.unwrap()
        };
    let original = binding(
        "alpha",
        "provider",
        "account",
        "https://facilitator.test",
        Mode::Production,
        7,
    );
    assert!(
        original
            == binding(
                "alpha",
                "provider",
                "account",
                "https://facilitator.test",
                Mode::Production,
                7
            )
    );
    for changed in [
        binding(
            "beta",
            "provider",
            "account",
            "https://facilitator.test",
            Mode::Production,
            7,
        ),
        binding(
            "alpha",
            "other",
            "account",
            "https://facilitator.test",
            Mode::Production,
            7,
        ),
        binding(
            "alpha",
            "provider",
            "other",
            "https://facilitator.test",
            Mode::Production,
            7,
        ),
        binding(
            "alpha",
            "provider",
            "account",
            "https://other.test",
            Mode::Production,
            7,
        ),
        binding(
            "alpha",
            "provider",
            "account",
            "https://facilitator.test",
            Mode::Sandbox,
            7,
        ),
        binding(
            "alpha",
            "provider",
            "account",
            "https://facilitator.test",
            Mode::Production,
            8,
        ),
    ] {
        assert!(changed != original);
    }
    // Tuple encoding distinguishes boundaries without a homemade concatenation.
    assert!(
        binding(
            "ab",
            "c",
            "account",
            "https://facilitator.test",
            Mode::Production,
            7
        ) != binding(
            "a",
            "bc",
            "account",
            "https://facilitator.test",
            Mode::Production,
            7
        )
    );
    for (scope, mode, key) in [
        ("", Mode::Production, 7),
        ("alpha", Mode::Simulation, 7),
        ("alpha", Mode::Production, 0),
    ] {
        let mut copy = [key; 32];
        assert!(
            account_identity(
                &mut copy,
                scope,
                "provider",
                "account",
                "https://facilitator.test",
                mode
            )
            .is_err()
        );
        assert_eq!(copy, [0; 32]);
    }
}

#[tokio::test]
async fn metadata_projection_preserves_real_execution_and_only_removes_provider_continuation() {
    let journal = MemoryJournal::default();
    let (payments, _) = crate::payments::tests::setup(journal.clone(), false);
    let instruction = crate::payments::tests::request();
    let execution = payments.execute(&instruction, 1).await.unwrap();
    assert_eq!(execution.outcome.state(), State::Collected);
    let (key, original) = {
        let state = journal.0.lock().unwrap();
        let (key, record) = state.records.iter().next().unwrap();
        (key.clone(), record.clone())
    };
    assert_eq!(
        original.continuation.as_deref(),
        Some(b"provider-reference".as_slice())
    );
    let projected = original.without_continuation();
    let mut expected = serde_json::to_value(&original).unwrap();
    expected["continuation"] = serde_json::Value::Null;
    assert_eq!(serde_json::to_value(&projected).unwrap(), expected);
    assert_eq!(projected.revision(), original.revision());
    assert_eq!(projected.receipt(), original.receipt());
    assert_eq!(projected.state(), original.state());
    assert_eq!(
        original.continuation.as_deref(),
        Some(b"provider-reference".as_slice())
    );
    let encoded = serde_json::to_vec(&projected).unwrap();
    for raw in [
        "provider-reference",
        "commercial-terms",
        "KYC-do-not-retain",
        "member-only",
        "full-private-provider-result",
    ] {
        assert!(!String::from_utf8_lossy(&encoded).contains(raw));
    }
    // Restore the genuine serialized projection into caller-owned state. It
    // preserves outcome/evidence and retry suppression, but cannot manufacture
    // the removed provider reference required for read-only reconciliation.
    let restored: Record = serde_json::from_slice(&encoded).unwrap();
    journal.0.lock().unwrap().records.insert(key, restored);
    assert_eq!(
        payments
            .execute(&instruction, 2)
            .await
            .unwrap()
            .outcome
            .state(),
        State::Collected
    );
    assert!(matches!(
        payments
            .observe(&instruction.id, &instruction.binding)
            .await,
        Err(Error::Unsupported)
    ));
    assert!(
        payments
            .outcome(&instruction.id, &instruction.binding)
            .await
            .unwrap()
            .evidence(&instruction.binding, State::Collected, 2, 50)
            .is_ok()
    );
}
