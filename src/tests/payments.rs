use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) fn request() -> Instruction {
    Instruction {
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

struct Provider {
    calls: AtomicUsize,
    error: bool,
}
impl Adapter for Provider {
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
        request: &Instruction,
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
            state: State::Collected,
            valid_until: Some(50),
            continuation: Some(Private::new(b"provider-reference".to_vec())),
            claim: Some(Private::new(b"test:receipt".to_vec())),
            member_action: Some(Private::new(b"member-only".to_vec())),
        })
    }
    async fn observe(&self, _: &str, continuation: Option<&Private>) -> Result<Observation> {
        Ok(Observation::fact(if continuation.is_some() {
            State::Refunded
        } else {
            State::Unknown
        }))
    }
}

fn setup(journal: MemoryJournal, error: bool) -> (Payments<MemoryJournal>, Arc<Provider>) {
    let mut payments = Payments::new(journal, [7; 32]).unwrap();
    let provider = Arc::new(Provider {
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
    assert_eq!(
        payments
            .observe(&id, &binding)
            .await
            .unwrap()
            .outcome
            .state(),
        State::Unknown
    );
    assert_eq!(
        payments
            .execute(&self::request(), 2)
            .await
            .unwrap()
            .outcome
            .state(),
        State::Unknown
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
        Payments::new(MemoryJournal::default(), [0; 32]),
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
    assert!(matches!(
        payments.execute(&repeated, 1).await,
        Err(Error::Conflict)
    ));
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
    assert!(matches!(
        payments.execute(&repeated, 1).await,
        Err(Error::Conflict)
    ));
    assert_eq!(journal.0.lock().unwrap().receipts.len(), 1);
}

struct SlowProvider {
    started: tokio::sync::Notify,
}
impl Adapter for SlowProvider {
    fn max_input_bytes(&self) -> usize {
        16384
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            id: "test".into(),
            operations: vec!["collect".into()],
        }
    }
    async fn execute(&self, _: &Instruction, _: &str, _: Option<&Private>) -> Result<Observation> {
        self.started.notify_one();
        std::future::pending().await
    }
    async fn observe(&self, _: &str, _: Option<&Private>) -> Result<Observation> {
        Ok(Observation::fact(State::Unknown))
    }
}

#[tokio::test]
async fn cancelling_an_inflight_request_preserves_its_uncertain_execution_marker() {
    let provider = Arc::new(SlowProvider {
        started: tokio::sync::Notify::new(),
    });
    let mut payments = Payments::new(MemoryJournal::default(), [7; 32]).unwrap();
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
    let mut reopened = Payments::new(journal, [8; 32]).unwrap();
    let provider = Arc::new(Provider {
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
    fn max_input_bytes(&self) -> usize {
        16384
    }
    fn capabilities(&self) -> Capabilities {
        self.0.clone()
    }
    async fn execute(&self, _: &Instruction, _: &str, _: Option<&Private>) -> Result<Observation> {
        Err(Error::Unsupported)
    }
    async fn observe(&self, _: &str, _: Option<&Private>) -> Result<Observation> {
        Err(Error::Unsupported)
    }
}

#[tokio::test]
async fn malformed_adapter_configuration_and_cross_provider_parents_are_rejected() {
    let (mut payments, _) = setup(MemoryJournal::default(), false);
    for (id, operations) in [("", vec!["collect"]), ("empty", vec![]), ("bad", vec![""])] {
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
    assert!(matches!(
        payments.observe(&request().id, &request().binding).await,
        Err(Error::Storage)
    ));
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
        journal.begin("key", &record).await,
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
