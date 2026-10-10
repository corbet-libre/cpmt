use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

pub(crate) fn request() -> Instruction {
    Instruction {
        kind: Action::Execute,
        id: [1; 32],
        parent: None,
        adapter: "test".into(),
        operation: "collect".into(),
        binding: Binding {
            scope: "community".into(),
            subject: "member".into(),
            action: "join".into(),
        },
        expires_at: 100,
        terms: Private::new(b"commercial-terms".to_vec()),
        evidence: Private::new(b"KYC-do-not-retain".to_vec()),
    }
}

pub(crate) struct Provider {
    fee: Option<cost::Amount>,
    calls: AtomicUsize,
    error: bool,
}
impl Adapter for Provider {
    fn mode(&self) -> Mode {
        Mode::Production
    }
    fn max_input_bytes(&self) -> usize {
        16384
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            id: "test".into(),
            operations: vec!["collect".into(), "refund".into()],
        }
    }
    async fn execute(
        &self,
        request: &Dispatch<'_>,
        key: &str,
        previous: Option<&Private>,
    ) -> Result<Observation> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(key.len(), 64);
        if self.error {
            return Err(Error::Rejected);
        }
        if request.parent.is_some() {
            assert_eq!(previous.unwrap().expose(), b"provider-reference");
        }
        Ok(Observation {
            satisfies_terms: true,
            actual_cost: self.fee.clone(),
            state: State::Collected,
            valid_until: Some(50),
            continuation: Some(Private::new(b"provider-reference".to_vec())),
            claim: Some(Private::new(b"test:receipt".to_vec())),
            member_action: Some(Private::new(b"member-only".to_vec())),
            upstream: vec![ProviderReply {
                phase: "response",
                headers: None,
                http_status: Some(200),
                payload: Private::new(b"full-private-provider-result".to_vec()),
            }],
        })
    }
    async fn observe(
        &self,
        _: &Dispatch<'_>,
        continuation: Option<&Private>,
    ) -> Result<Observation> {
        Ok(Observation::fact(if continuation.is_some() {
            State::Refunded
        } else {
            State::Unknown
        }))
    }
}

pub(crate) fn setup(
    journal: MemoryJournal,
    error: bool,
) -> (Payments<MemoryJournal>, Arc<Provider>) {
    let mut payments = Payments::new(journal, [7; 32], std::sync::Arc::new(|| 0)).unwrap();
    let provider = Arc::new(Provider {
        fee: None,
        calls: AtomicUsize::new(0),
        error,
    });
    payments.register(provider.clone()).unwrap();
    (payments, provider)
}

#[tokio::test]
async fn concurrent_and_restarted_execution_contacts_provider_once() {
    let journal = MemoryJournal::default();
    let (payments, provider) = setup(journal.clone(), false);
    let request = request();
    let (first, second) =
        tokio::join!(payments.execute(&request, 1), payments.execute(&request, 1));
    assert_eq!(
        first.unwrap().member_action.unwrap().expose(),
        b"member-only"
    );
    assert!(second.unwrap().member_action.is_none());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let (reopened, after_restart) = setup(journal.clone(), false);
    assert_eq!(
        reopened.execute(&request, 1).await.unwrap().outcome.state(),
        State::Collected
    );
    assert_eq!(after_restart.calls.load(Ordering::SeqCst), 0);
    let wire =
        serde_json::to_string(&journal.0.lock().unwrap().records.values().next().unwrap()).unwrap();
    assert!(!wire.contains("KYC"));
    assert!(!wire.contains("commercial-terms"));
    assert!(!wire.contains("member-only"));
    assert_eq!(payments.capabilities()[0].operations, ["collect", "refund"]);
}

#[tokio::test]
async fn changed_terms_or_member_cannot_reuse_an_operation() {
    let (payments, _) = setup(MemoryJournal::default(), false);
    payments.execute(&request(), 1).await.unwrap();
    let mut altered = request();
    altered.terms = Private::new(b"different".to_vec());
    assert!(matches!(
        payments.execute(&altered, 1).await,
        Err(Error::Conflict)
    ));
    altered = request();
    altered.binding.subject = "other".into();
    assert!(matches!(
        payments.execute(&altered, 1).await,
        Err(Error::Conflict)
    ));
    assert!(matches!(
        payments.outcome(&altered.id, &altered.binding).await,
        Err(Error::Rejected)
    ));
    assert!(matches!(
        payments.observe(&altered.id, &altered.binding).await,
        Err(Error::Rejected)
    ));
}

#[tokio::test]
async fn raw_input_can_be_dropped_before_reconciliation_and_forgetting() {
    let journal = MemoryJournal::default();
    let (payments, _) = setup(journal.clone(), false);
    let request = request();
    let id = request.id;
    let binding = request.binding.clone();
    let outcome = payments.execute(&request, 1).await.unwrap().outcome;
    drop(request);
    assert_eq!(outcome.adapter(), "test");
    assert_eq!(outcome.operation(), "collect");
    assert_eq!(
        payments
            .observe(&id, &binding)
            .await
            .unwrap()
            .outcome
            .state(),
        State::Refunded
    );
    payments.forget(&id, &binding).await.unwrap();
    assert!(
        journal
            .0
            .lock()
            .unwrap()
            .records
            .values()
            .next()
            .unwrap()
            .continuation
            .is_none()
    );
    assert!(matches!(
        payments.observe(&id, &binding).await,
        Err(Error::Unsupported)
    ));
    assert_eq!(
        payments
            .execute(&self::request(), 2)
            .await
            .unwrap()
            .outcome
            .state(),
        State::Collected
    );
}

#[tokio::test]
async fn uncertain_execution_never_becomes_a_second_charge() {
    let (payments, provider) = setup(MemoryJournal::default(), true);
    assert!(matches!(
        payments.execute(&request(), 1).await,
        Err(Error::Rejected)
    ));
    let retried = payments.execute(&request(), 1).await.unwrap();
    assert_eq!(retried.outcome.state(), State::Unknown);
    assert!(
        retried
            .outcome
            .evidence(&request().binding, State::Unknown, 1, 20)
            .is_err()
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn observing_an_unknown_operation_does_not_insert_it() {
    let journal = MemoryJournal::default();
    let (payments, _) = setup(journal.clone(), false);
    let request = request();
    assert!(matches!(
        payments.observe(&request.id, &request.binding).await,
        Err(Error::Rejected)
    ));
    assert!(journal.0.lock().unwrap().records.is_empty());
    assert_eq!(
        payments.execute(&request, 1).await.unwrap().outcome.state(),
        State::Collected
    );
}

#[tokio::test]
async fn gate_evidence_binds_member_action_scope_provider_and_exclusive_expiry() {
    let (payments, _) = setup(MemoryJournal::default(), false);
    let request = request();
    let outcome = payments.execute(&request, 1).await.unwrap().outcome;
    let proof = outcome
        .evidence(&request.binding, State::Collected, 1, 100)
        .unwrap();
    assert_eq!(
        proof.in_context(&request.binding, 49).unwrap(),
        ("test", 50)
    );
    assert_ne!(proof.claim(), &request.id);
    assert_eq!(
        proof.in_context(&request.binding, 50).unwrap_err(),
        Error::Expired
    );
    for binding in [
        Binding {
            subject: "other".into(),
            ..request.binding.clone()
        },
        Binding {
            scope: "other".into(),
            ..request.binding.clone()
        },
        Binding {
            action: "other".into(),
            ..request.binding.clone()
        },
    ] {
        assert!(matches!(
            outcome.evidence(&binding, State::Collected, 1, 20),
            Err(Error::Rejected)
        ));
        assert_eq!(proof.in_context(&binding, 1).unwrap_err(), Error::Rejected);
    }
    assert!(matches!(
        outcome.evidence(&request.binding, State::Authorized, 1, 20),
        Err(Error::Rejected)
    ));
    assert!(matches!(
        outcome.evidence(&request.binding, State::Collected, 20, 20),
        Err(Error::Expired)
    ));
    for state in [
        State::Unknown,
        State::Pending,
        State::RequiresAction,
        State::Failed,
    ] {
        let mut outcome = outcome.clone();
        outcome.0.state = state;
        assert!(outcome.evidence(&request.binding, state, 1, 20).is_err());
    }
}

#[tokio::test]
async fn followup_operations_use_only_the_same_members_provider_reference() {
    let (payments, _) = setup(MemoryJournal::default(), false);
    payments.execute(&request(), 1).await.unwrap();
    let mut child = request();
    child.id = [2; 32];
    child.parent = Some([1; 32]);
    child.operation = "refund".into();
    payments.execute(&child, 1).await.unwrap();
    child.id = [3; 32];
    child.binding.subject = "other".into();
    assert!(matches!(
        payments.execute(&child, 1).await,
        Err(Error::Rejected)
    ));
    child.binding = request().binding;
    child.id = [4; 32];
    child.parent = Some(child.id);
    assert!(matches!(
        payments.execute(&child, 1).await,
        Err(Error::Rejected)
    ));
    payments
        .forget(&request().id, &request().binding)
        .await
        .unwrap();
    child.id = [5; 32];
    child.parent = Some(request().id);
    assert!(matches!(
        payments.execute(&child, 1).await,
        Err(Error::Rejected)
    ));
}

#[tokio::test]
async fn invalid_and_expired_requests_never_contact_a_provider() {
    let (payments, provider) = setup(MemoryJournal::default(), false);
    assert!(matches!(
        payments.execute(&request(), 100).await,
        Err(Error::Expired)
    ));
    let mut bad = request();
    bad.id = [0; 32];
    assert!(matches!(
        payments.execute(&bad, 1).await,
        Err(Error::Invalid)
    ));
    bad = request();
    bad.expires_at = 0;
    assert!(matches!(
        payments.execute(&bad, 1).await,
        Err(Error::Invalid)
    ));
    for field in ["", "bad\n", &"x".repeat(257)] {
        bad = request();
        bad.binding.scope = field.into();
        assert!(matches!(
            payments.execute(&bad, 1).await,
            Err(Error::Invalid)
        ));
    }
    bad = request();
    bad.adapter = "absent".into();
    assert!(matches!(
        payments.execute(&bad, 1).await,
        Err(Error::Unsupported)
    ));
    bad = request();
    bad.operation = "absent".into();
    assert!(matches!(
        payments.execute(&bad, 1).await,
        Err(Error::Unsupported)
    ));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn registration_storage_conflicts_and_diagnostics_are_safe() {
    assert!(matches!(
        Payments::new(MemoryJournal::default(), [0; 32], std::sync::Arc::new(|| 0)),
        Err(Error::Invalid)
    ));
    let journal = MemoryJournal::default();
    let (mut payments, provider) = setup(journal.clone(), false);
    assert_eq!(payments.register(provider).unwrap_err(), Error::Conflict);
    let request = request();
    let outcome = payments.execute(&request, 1).await.unwrap().outcome;
    let record = journal
        .0
        .lock()
        .unwrap()
        .records
        .values()
        .next()
        .unwrap()
        .clone();
    assert_eq!(
        journal.replace("absent", 0, &record).await.unwrap_err(),
        Error::Conflict
    );
    for debug in [
        format!("{:?}", request),
        format!("{:?}", request.evidence),
        format!("{:?}", request.binding),
        format!("{outcome:?}"),
        format!("{record:?}"),
    ] {
        assert!(!debug.contains("member"));
        assert!(!debug.contains("KYC"));
        assert!(!debug.contains("reference"));
    }
    payments.adapters.clear();
    assert!(matches!(
        payments.observe(&request.id, &request.binding).await,
        Err(Error::Unsupported)
    ));
}

#[tokio::test]
async fn one_receipt_cannot_fund_another_member_or_action() {
    let journal = MemoryJournal::default();
    let (payments, _) = setup(journal.clone(), false);
    let first = payments.execute(&request(), 1).await.unwrap().outcome;
    let mut repeated = request();
    repeated.id = [2; 32];
    let second = payments.execute(&repeated, 1).await.unwrap().outcome;
    assert_eq!(
        first
            .evidence(&request().binding, State::Collected, 1, 20)
            .unwrap()
            .claim(),
        second
            .evidence(&repeated.binding, State::Collected, 1, 20)
            .unwrap()
            .claim()
    );
    repeated.id = [3; 32];
    repeated.binding.subject = "other-member".into();
    let conflicted = payments.execute(&repeated, 1).await.unwrap();
    assert_eq!(conflicted.commit_error, Some(Error::Conflict));
    assert_eq!(conflicted.outcome.state(), State::Unknown);
    assert!(conflicted.member_action.is_none());
    assert_eq!(
        payments
            .outcome(&repeated.id, &repeated.binding)
            .await
            .unwrap()
            .state(),
        State::Unknown
    );
    repeated.id = [4; 32];
    repeated.binding = request().binding;
    repeated.binding.action = "other-action".into();
    let conflicted = payments.execute(&repeated, 1).await.unwrap();
    assert_eq!(conflicted.commit_error, Some(Error::Conflict));
    assert_eq!(conflicted.outcome.state(), State::Unknown);
    assert!(conflicted.member_action.is_none());
    assert_eq!(journal.0.lock().unwrap().receipts.len(), 1);
}

struct SlowProvider {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl Adapter for SlowProvider {
    fn mode(&self) -> Mode {
        Mode::Production
    }
    fn max_input_bytes(&self) -> usize {
        16384
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            id: "test".into(),
            operations: vec!["collect".into()],
        }
    }
    async fn execute(&self, _: &Dispatch<'_>, _: &str, _: Option<&Private>) -> Result<Observation> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(Observation {
            continuation: Some(Private::new(b"provider-reference".to_vec())),
            claim: Some(Private::new(b"slow:receipt".to_vec())),
            ..Observation::fact(State::Collected)
        })
    }
    async fn observe(&self, _: &Dispatch<'_>, _: Option<&Private>) -> Result<Observation> {
        Ok(Observation::fact(State::Unknown))
    }
}

#[tokio::test]
async fn cancelling_an_inflight_request_preserves_its_uncertain_execution_marker() {
    let provider = Arc::new(SlowProvider {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let mut payments =
        Payments::new(MemoryJournal::default(), [7; 32], std::sync::Arc::new(|| 0)).unwrap();
    payments.register(provider.clone()).unwrap();
    let payments = Arc::new(payments);
    let worker = payments.clone();
    let task = tokio::spawn(async move { worker.execute(&request(), 1).await });
    provider.started.notified().await;
    task.abort();
    assert!(matches!(task.await,Err(error) if error.is_cancelled()));
    assert_eq!(
        payments
            .execute(&request(), 1)
            .await
            .unwrap()
            .outcome
            .state(),
        State::Unknown
    );
}

#[tokio::test]
async fn forgetting_a_parent_does_not_invalidate_a_completed_child_retry() {
    let (payments, provider) = setup(MemoryJournal::default(), false);
    payments.execute(&request(), 1).await.unwrap();
    let mut child = request();
    child.id = [2; 32];
    child.parent = Some([1; 32]);
    child.operation = "refund".into();
    payments.execute(&child, 1).await.unwrap();
    payments
        .forget(&request().id, &request().binding)
        .await
        .unwrap();
    assert_eq!(
        payments.execute(&child, 2).await.unwrap().outcome.state(),
        State::Collected
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn changing_the_binding_key_cannot_rearm_a_payment_id() {
    let journal = MemoryJournal::default();
    let (payments, _) = setup(journal.clone(), false);
    payments.execute(&request(), 1).await.unwrap();
    let mut reopened = Payments::new(journal, [8; 32], std::sync::Arc::new(|| 0)).unwrap();
    let provider = Arc::new(Provider {
        fee: None,
        calls: AtomicUsize::new(0),
        error: false,
    });
    reopened.register(provider.clone()).unwrap();
    assert!(matches!(
        reopened.execute(&request(), 1).await,
        Err(Error::KeyMismatch)
    ));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

struct Configured(Capabilities);
impl Adapter for Configured {
    fn mode(&self) -> Mode {
        Mode::Production
    }
    fn max_input_bytes(&self) -> usize {
        if self.0.id == "unbounded" { 0 } else { 16384 }
    }
    fn capabilities(&self) -> Capabilities {
        self.0.clone()
    }
    async fn execute(&self, _: &Dispatch<'_>, _: &str, _: Option<&Private>) -> Result<Observation> {
        Err(Error::Unsupported)
    }
    async fn observe(&self, _: &Dispatch<'_>, _: Option<&Private>) -> Result<Observation> {
        Err(Error::Unsupported)
    }
}

#[tokio::test]
async fn malformed_adapter_configuration_and_cross_provider_parents_are_rejected() {
    let (mut payments, _) = setup(MemoryJournal::default(), false);
    for (id, operations) in [
        ("", vec!["collect"]),
        ("empty", vec![]),
        ("bad", vec![""]),
        ("unbounded", vec!["collect"]),
    ] {
        assert_eq!(
            payments
                .register(Arc::new(Configured(Capabilities {
                    id: id.into(),
                    operations: operations.into_iter().map(String::from).collect()
                })))
                .unwrap_err(),
            Error::Invalid
        );
    }
    payments
        .register(Arc::new(Configured(Capabilities {
            id: "other".into(),
            operations: vec!["collect".into()],
        })))
        .unwrap();
    payments.execute(&request(), 1).await.unwrap();
    let mut child = request();
    child.id = [2; 32];
    child.parent = Some([1; 32]);
    child.adapter = "other".into();
    assert!(matches!(
        payments.execute(&child, 1).await,
        Err(Error::Rejected)
    ));
}

#[tokio::test]
async fn poisoned_journals_and_exhausted_revisions_never_grant_success() {
    let journal = MemoryJournal::default();
    let (payments, _) = setup(journal.clone(), false);
    let mut outcome = payments.execute(&request(), 1).await.unwrap().outcome;
    outcome.0.receipt = None;
    outcome.0.valid_until = None;
    assert!(
        outcome
            .evidence(&request().binding, State::Collected, 1, 20)
            .is_err()
    );
    let record = {
        let mut state = journal.0.lock().unwrap();
        let record = state.records.values_mut().next().unwrap();
        assert_eq!(record.revision(), 1);
        record.revision = u64::MAX;
        record.clone()
    };
    // A read has its own immutable identity and revision; it cannot mutate or
    // overflow the original operation's exhausted revision.
    assert_eq!(
        payments
            .observe(&request().id, &request().binding)
            .await
            .unwrap()
            .outcome
            .state(),
        State::Refunded
    );
    assert!(
        journal
            .0
            .lock()
            .unwrap()
            .records
            .values()
            .any(|r| r.revision() == u64::MAX)
    );
    assert_eq!(
        payments
            .forget(&request().id, &request().binding)
            .await
            .unwrap_err(),
        Error::Storage
    );
    let shared = journal.clone();
    assert!(
        std::thread::spawn(move || {
            let _guard = shared.0.lock().unwrap();
            panic!("fixture interrupted writer");
        })
        .join()
        .is_err()
    );
    assert_eq!(journal.bind(&[7; 32]).await.unwrap_err(), Error::Storage);
    assert!(matches!(journal.get("key").await, Err(Error::Storage)));
    assert!(matches!(
        journal.begin("key", &record, 100, &|| 0).await,
        Err(Error::Storage)
    ));
    assert_eq!(
        journal.replace("key", 0, &record).await.unwrap_err(),
        Error::Storage
    );
}

#[tokio::test]
async fn adapter_input_limits_apply_before_journaling_or_dispatch() {
    let journal = MemoryJournal::default();
    let (payments, provider) = setup(journal.clone(), false);
    let mut request = request();
    request.evidence = Private::new(vec![0; 16384]);
    assert!(matches!(
        payments.execute(&request, 1).await,
        Err(Error::Invalid)
    ));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert!(journal.0.lock().unwrap().records.is_empty());
}

#[tokio::test]
async fn polling_or_forgetting_an_inflight_payment_cannot_discard_its_provider_result() {
    let provider = Arc::new(SlowProvider {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let journal = MemoryJournal::default();
    let mut payments = Payments::new(journal.clone(), [7; 32], std::sync::Arc::new(|| 0)).unwrap();
    payments.register(provider.clone()).unwrap();
    let payments = Arc::new(payments);
    let worker = payments.clone();
    let task = tokio::spawn(async move { worker.execute(&request(), 1).await });
    provider.started.notified().await;
    let request = request();
    assert!(matches!(
        payments.observe(&request.id, &request.binding).await,
        Err(Error::Unsupported)
    ));
    assert_eq!(
        payments
            .forget(&request.id, &request.binding)
            .await
            .unwrap_err(),
        Error::Conflict
    );
    provider.release.notify_one();
    assert_eq!(
        task.await.unwrap().unwrap().outcome.state(),
        State::Collected
    );
    assert_eq!(
        payments
            .outcome(&request.id, &request.binding)
            .await
            .unwrap()
            .state(),
        State::Collected
    );
    assert!(
        journal
            .0
            .lock()
            .unwrap()
            .records
            .values()
            .next()
            .unwrap()
            .continuation
            .is_some()
    );
}

fn priced(request: &Instruction, mode: Mode, atoms: u64) -> DispatchPermit<String> {
    let amount = cost::Amount {
        unit: cost::Unit {
            code: "fee".into(),
            scale: 2,
        },
        atoms,
    };
    cost::Plan::new(
        vec![cost::Offer {
            candidate: cfbk::Candidate::new(request.adapter.clone(), [request.operation.clone()]),
            quote: Quote {
                price: Price::AtMost(amount.clone()),
                source: "fixture tariff v1".into(),
                valid_until: 99,
                binding: cost::binding(request, mode),
            },
        }],
        cfbk::Config::new([request.adapter.clone()], [request.operation.clone()]),
        cost::CostOrder::Configured,
        amount,
        1,
        mode,
        0,
    )
    .unwrap()
    .start()
    .begin(0)
    .unwrap()
}

#[tokio::test]
async fn rich_private_results_are_opt_in_and_never_enter_caller_state() {
    let journal = MemoryJournal::default();
    let (payments, _) = setup(journal.clone(), false);
    let mut request = request();
    let ordinary = payments.execute(&request, 1).await.unwrap();
    assert!(ordinary.upstream.is_empty());
    request.id = [44; 32];
    let full = payments
        .dispatch_with_output(
            &request,
            priced(&request, Mode::Production, 2),
            1,
            Disclosure::Complete,
        )
        .await
        .unwrap();
    assert_eq!(
        full.upstream[0].payload.expose(),
        b"full-private-provider-result"
    );
    assert!(!format!("{:?}", full.upstream).contains("private-provider"));
    {
        let stored = journal.0.lock().unwrap();
        for record in stored.records.values() {
            let json = serde_json::to_string(record).unwrap();
            assert!(!json.contains("full-private-provider"));
            assert!(!json.contains("member-only"));
        }
    }
    let replay = payments
        .dispatch_with_output(
            &request,
            priced(&request, Mode::Production, 2),
            1,
            Disclosure::Complete,
        )
        .await
        .unwrap();
    assert!(replay.upstream.is_empty());
}

#[tokio::test]
async fn changed_terms_modes_candidates_and_expired_quotes_cannot_dispatch() {
    let (payments, provider) = setup(MemoryJournal::default(), false);
    let mut request = request();
    let old = priced(&request, Mode::Production, 2);
    request.terms = Private::new(b"changed".to_vec());
    assert!(matches!(
        payments.dispatch(&request, old, 1).await,
        Err(Error::Cost)
    ));
    for mode in [Mode::Simulation, Mode::Sandbox] {
        assert!(matches!(
            payments
                .dispatch(&request, priced(&request, mode, 2), 1)
                .await,
            Err(Error::Cost)
        ));
    }
    assert!(matches!(
        payments
            .dispatch(&request, priced(&request, Mode::Production, 2), 99)
            .await,
        Err(Error::Cost)
    ));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let quote = payments
        .preview(&request, Price::Unknown, "no tariff".into(), 1000)
        .unwrap();
    assert_eq!(quote.price, Price::Unknown);
    assert_eq!(quote.valid_until, request.expires_at);
    let permit = priced(&request, Mode::Production, 2);
    assert!(cost::validate(&request, "different-account", Mode::Production, &permit, 1).is_err());
    request.kind = Action::Observe;
    assert!(
        payments
            .preview(&request, Price::Unknown, "source".into(), 99)
            .is_err()
    );
}

#[tokio::test]
async fn atomic_caller_budget_reservation_survives_concurrency_and_retries() {
    let amount = cost::Amount {
        unit: cost::Unit {
            code: "fee".into(),
            scale: 2,
        },
        atoms: 10,
    };
    let journal = MemoryJournal::with_budget(amount);
    let (payments, provider) = setup(journal.clone(), false);
    let first = request();
    let mut second = request();
    second.id = [2; 32];
    let (a, b) = tokio::join!(
        payments.dispatch(&first, priced(&first, Mode::Production, 7), 1),
        payments.dispatch(&second, priced(&second, Mode::Production, 7), 1)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let winner = if a.is_ok() { first } else { second };
    let again = payments
        .dispatch(&winner, priced(&winner, Mode::Production, 7), 2)
        .await
        .unwrap();
    assert_eq!(again.cost.maximum, 7);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let record = journal
        .0
        .lock()
        .unwrap()
        .records
        .values()
        .next()
        .unwrap()
        .clone();
    assert_eq!(record.cost().maximum, 7);
    assert_eq!(record.state(), State::Collected);
    let mut read = payments
        .observation(&winner.id, &winner.binding, [3; 32], 100)
        .await
        .unwrap();
    assert!(matches!(
        payments
            .dispatch(&read, priced(&read, Mode::Production, 7), 1)
            .await,
        Err(Error::Cost)
    ));
    read.id = [4; 32];
    assert!(matches!(
        payments
            .dispatch(
                &read,
                crate::test_support::permit(&read, Mode::Production),
                1
            )
            .await,
        Err(Error::Cost)
    ));
}

#[test]
fn actual_cost_never_exceeds_reserved_units_and_fallback_keeps_uncertainty() {
    let request = request();
    let mut reserved = Reservation::from_permit(&priced(&request, Mode::Production, 7)).unwrap();
    let mut actual = cost::Amount {
        unit: cost::Unit {
            code: "fee".into(),
            scale: 2,
        },
        atoms: 6,
    };
    reserved.report(Some(&actual)).unwrap();
    assert_eq!(reserved.actual, Some(6));
    actual.atoms = 8;
    assert!(reserved.report(Some(&actual)).is_err());
    actual.atoms = 1;
    actual.unit.scale = 0;
    assert!(reserved.report(Some(&actual)).is_err());
    actual.unit.scale = 2;
    actual.unit.code = "other".into();
    assert!(reserved.report(Some(&actual)).is_err());
    for (state, resolution) in [
        (State::Unknown, cost::Outcome::Uncertain),
        (State::Pending, cost::Outcome::Pending),
        (State::RequiresAction, cost::Outcome::Pending),
        (State::Failed, cost::Outcome::SafeFailure),
        (State::Collected, cost::Outcome::Success),
    ] {
        assert_eq!(crate::run::classify(state, state), resolution);
    }
}

#[tokio::test]
async fn snapshots_reads_and_caller_budget_keep_environment_and_unknown_exposure() {
    let journal = MemoryJournal::default();
    let (payments, _) = setup(journal.clone(), false);
    let request = request();
    let mut outcome = payments.execute(&request, 1).await.unwrap().outcome;
    assert!(outcome.satisfies_terms());
    outcome.0.sandbox = true;
    assert_eq!(outcome.mode(), Mode::Sandbox);
    assert!(
        outcome
            .evidence(&request.binding, State::Collected, 1, 30)
            .is_err()
    );
    outcome.0.id = [8; 32];
    assert!(!outcome.matches_request(&request, outcome.0.request_digest));
    outcome.0.id = request.id;
    outcome.0.binding.subject.push('x');
    assert!(!outcome.matches_request(&request, outcome.0.request_digest));
    for evidence in [false, true] {
        let mut read = payments
            .observation(&request.id, &request.binding, [8; 32], 100)
            .await
            .unwrap();
        if evidence {
            read.evidence = Private::new(vec![1]);
        } else {
            read.terms = Private::new(vec![1]);
        }
        assert!(
            payments
                .preview(&read, Price::Unknown, "fixture".into(), 100)
                .is_err()
        );
    }
    let mut read = payments
        .observation(&request.id, &request.binding, [9; 32], 100)
        .await
        .unwrap();
    read.operation = "refund".into();
    assert_eq!(
        payments.execute(&read, 1).await.err(),
        Some(Error::Rejected)
    );
    {
        journal
            .0
            .lock()
            .unwrap()
            .records
            .values_mut()
            .find(|r| r.outcome.id == request.id)
            .unwrap()
            .outcome
            .sandbox = true;
    }
    let read = payments
        .observation(&request.id, &request.binding, [10; 32], 100)
        .await
        .unwrap();
    assert_eq!(
        payments.execute(&read, 1).await.err(),
        Some(Error::Rejected)
    );
    payments
        .forget(&request.id, &request.binding)
        .await
        .unwrap();
    payments
        .forget(&request.id, &request.binding)
        .await
        .unwrap();
    let budget = cost::Amount {
        unit: cost::Unit {
            code: "fee".into(),
            scale: 2,
        },
        atoms: 10,
    };
    let (uncertain, calls) = setup(MemoryJournal::with_budget(budget.clone()), true);
    assert_eq!(
        uncertain
            .dispatch(&request, priced(&request, Mode::Production, 7), 1)
            .await
            .err(),
        Some(Error::Rejected)
    );
    let mut second = self::request();
    second.id = [11; 32];
    assert_eq!(
        uncertain
            .dispatch(&second, priced(&second, Mode::Production, 7), 1)
            .await
            .err(),
        Some(Error::Cost)
    );
    assert_eq!(calls.calls.load(Ordering::SeqCst), 1);
    let mut wrong = budget;
    wrong.unit.scale = 1;
    let (other, _) = setup(MemoryJournal::with_budget(wrong), false);
    assert_eq!(
        other
            .dispatch(&request, priced(&request, Mode::Production, 7), 1)
            .await
            .err(),
        Some(Error::Cost)
    );
    let permit = priced(&request, Mode::Simulation, 0);
    assert!(cost::validate(&request, "test", Mode::Simulation, &permit, 1).is_err());
    let mut zero = self::request();
    zero.id = [0; 32];
    let permit = priced(&zero, Mode::Production, 0);
    assert!(cost::validate(&zero, "test", Mode::Production, &permit, 1).is_err());
}

struct DelayedJournal {
    inner: MemoryJournal,
    now: Arc<std::sync::atomic::AtomicU64>,
    before_reservation: bool,
    fail_commit: bool,
}
impl Journal for DelayedJournal {
    async fn bind(&self, key: &[u8; 32]) -> Result<()> {
        self.inner.bind(key).await
    }
    async fn get(&self, key: &str) -> Result<Option<Record>> {
        self.inner.get(key).await
    }
    async fn begin(
        &self,
        key: &str,
        record: &Record,
        deadline: u64,
        clock: &dyn cost::Clock,
    ) -> Result<Option<Record>> {
        assert_eq!(deadline, 99);
        if self.before_reservation {
            self.now.store(99, Ordering::SeqCst);
        }
        let record = self.inner.begin(key, record, deadline, clock).await?;
        if !self.fail_commit {
            self.now.store(99, Ordering::SeqCst);
        }
        Ok(record)
    }
    async fn replace(&self, key: &str, revision: u64, record: &Record) -> Result<()> {
        if self.fail_commit {
            Err(Error::Storage)
        } else {
            self.inner.replace(key, revision, record).await
        }
    }
}
#[tokio::test]
async fn reservation_delays_cannot_dispatch_expired_quotes() {
    for before in [true, false] {
        let now = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let clock = now.clone();
        let journal = DelayedJournal {
            inner: MemoryJournal::default(),
            now: now.clone(),
            before_reservation: before,
            fail_commit: false,
        };
        let inner = journal.inner.clone();
        let provider = Arc::new(Provider {
            fee: None,
            calls: AtomicUsize::new(0),
            error: false,
        });
        let mut payments = Payments::new(
            journal,
            [7; 32],
            Arc::new(move || clock.load(Ordering::SeqCst)),
        )
        .unwrap();
        payments.register(provider.clone()).unwrap();
        let request = request();
        let result = payments
            .dispatch(&request, priced(&request, Mode::Production, 7), 1)
            .await;
        assert_eq!(
            result.err(),
            Some(if before { Error::Expired } else { Error::Cost })
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(inner.0.lock().unwrap().records.len(), usize::from(!before));
    }
}
#[tokio::test]
async fn unexpected_charges_are_preserved_without_proof_or_further_budget_reservation() {
    for (unit, scale, atoms) in [("fee", 2, 8), ("other", 2, 3), ("fee", 3, 3)] {
        let journal = MemoryJournal::default();
        let mut payments = Payments::new(journal.clone(), [7; 32], Arc::new(|| 1)).unwrap();
        let fee = cost::Amount {
            unit: cost::Unit {
                code: unit.into(),
                scale,
            },
            atoms,
        };
        let provider = Arc::new(Provider {
            fee: Some(fee.clone()),
            calls: AtomicUsize::new(0),
            error: false,
        });
        payments.register(provider.clone()).unwrap();
        let request = request();
        let result = payments
            .dispatch_with_output(
                &request,
                priced(&request, Mode::Production, 7),
                1,
                Disclosure::Complete,
            )
            .await
            .unwrap();
        assert_eq!(result.cost.violation.as_ref().unwrap().amount(), fee);
        assert!(result.commit_error.is_none());
        assert_eq!(result.upstream.len(), 1);
        assert!(!result.outcome.satisfies_terms());
        assert!(
            result
                .outcome
                .evidence(&request.binding, State::Collected, 1, 50)
                .is_err()
        );
        let replay = payments
            .dispatch(&request, priced(&request, Mode::Production, 7), 1)
            .await
            .unwrap();
        assert_eq!(replay.cost.violation.unwrap().amount(), fee);
        let mut other = self::request();
        other.id = [9; 32];
        assert_eq!(
            payments
                .dispatch(&other, priced(&other, Mode::Production, 1), 1)
                .await
                .err(),
            Some(Error::Cost)
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn failed_caller_commit_returns_unknown_with_private_results_and_unbudgeted_charge() {
    let now = Arc::new(std::sync::atomic::AtomicU64::new(1));
    let clock = now.clone();
    let journal = DelayedJournal {
        inner: MemoryJournal::default(),
        now,
        before_reservation: false,
        fail_commit: true,
    };
    let mut payments = Payments::new(
        journal,
        [7; 32],
        Arc::new(move || clock.load(Ordering::SeqCst)),
    )
    .unwrap();
    let fee = cost::Amount {
        unit: cost::Unit {
            code: "other-fee".into(),
            scale: 0,
        },
        atoms: 8,
    };
    payments
        .register(Arc::new(Provider {
            fee: Some(fee.clone()),
            calls: AtomicUsize::new(0),
            error: false,
        }))
        .unwrap();
    let request = request();
    let result = payments
        .dispatch_with_output(
            &request,
            priced(&request, Mode::Production, 7),
            1,
            Disclosure::Complete,
        )
        .await
        .unwrap();
    assert_eq!(result.commit_error, Some(Error::Storage));
    assert_eq!(result.outcome.state(), State::Unknown);
    assert!(result.member_action.is_none());
    assert_eq!(result.upstream.len(), 1);
    assert_eq!(result.cost.violation.unwrap().amount(), fee);
    assert!(!result.outcome.satisfies_terms());
    assert_eq!(
        payments
            .outcome(&request.id, &request.binding)
            .await
            .unwrap()
            .state(),
        State::Unknown
    );
}
