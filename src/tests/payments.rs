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
#[async_trait]
impl Adapter for Provider {
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
    child.parent = Some(child.id);
    assert!(matches!(
        payments.execute(&child, 1).await,
        Err(Error::Rejected)
    ));
    payments
        .forget(&request().id, &request().binding)
        .await
        .unwrap();
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
