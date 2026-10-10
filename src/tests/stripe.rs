use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn server(replies: Vec<&str>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    server_status(replies.into_iter().map(|r| (200, r.to_owned())).collect()).await
}
async fn server_status(
    replies: Vec<(u16, String)>,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, reply) in replies {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let n = stream.read(&mut buffer).await.unwrap();
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..n]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: "))
                        .map_or(0, |n| n.parse::<usize>().unwrap());
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            if !reply.is_empty() {
                stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",reply.len(),reply).as_bytes()).await.unwrap();
            }
        }
        requests
    });
    (url, task)
}

fn adapter() -> Stripe {
    configured(
        "stripe".into(),
        Private::new(b"sk_test_fixture".to_vec()),
        Duration::from_secs(2),
        8192,
    )
    .unwrap()
}
fn request(op: &str, terms: Value) -> Instruction {
    Instruction {
        kind: Action::Execute,
        id: [1; 32],
        parent: None,
        adapter: "stripe".into(),
        operation: op.into(),
        binding: Binding {
            scope: "c".into(),
            subject: "m".into(),
            action: "a".into(),
        },
        expires_at: 100,
        terms: Private::new(serde_json::to_vec(&terms).unwrap()),
        evidence: Private::new(vec![]),
    }
}
const HELD: &str = r#"{"id":"pi_fixture","object":"payment_intent","status":"requires_capture","billing_details":{"name":"DO NOT RETAIN"}}"#;
const PAID: &str = r#"{"id":"pi_fixture","object":"payment_intent","status":"succeeded"}"#;

#[tokio::test]
async fn sdk_creates_hold_captures_and_observes_without_repeating_a_charge() {
    let (url, calls) = server(vec![HELD, PAID, PAID]).await;
    let mut adapter = adapter();
    adapter.client = ClientBuilder::new("sk_test_fixture")
        .url(format!("{url}/"))
        .build()
        .unwrap();
    assert_eq!(adapter.capabilities().operations.len(), 8);
    assert_eq!(adapter.max_input_bytes(), 8192);
    let held = adapter
        .execute(
            &request(
                "stripe.intent.create",
                serde_json::json!({"amount":1200,"currency":"chf","capture_method":"manual"}),
            ),
            "operation-key",
            None,
        )
        .await
        .unwrap();
    assert_eq!(held.state, State::Authorized);
    let reference = held.continuation.unwrap();
    assert!(!String::from_utf8_lossy(reference.expose()).contains("billing"));
    let paid = adapter
        .execute(
            &request("stripe.intent.capture", serde_json::json!({})),
            "capture-key",
            Some(&reference),
        )
        .await
        .unwrap();
    assert_eq!(paid.state, State::Collected);
    assert_eq!(
        adapter
            .observe("stripe.intent.capture", paid.continuation.as_ref())
            .await
            .unwrap()
            .state,
        State::Collected
    );
    let calls = calls.await.unwrap();
    assert!(calls[0].starts_with("POST /v1/payment_intents "));
    assert!(
        calls[0]
            .to_ascii_lowercase()
            .contains("idempotency-key: operation-key")
    );
    assert!(calls[0].contains("currency=chf"));
    assert!(calls[1].starts_with("POST /v1/payment_intents/pi_fixture/capture "));
    assert_eq!(calls[1].split_once("\r\n\r\n").unwrap().1, "");
    assert!(calls[2].starts_with("GET /v1/payment_intents/pi_fixture "));
}

#[tokio::test]
async fn refunds_and_recurring_arrangements_report_distinct_facts() {
    let (url, calls) = server(vec![
        r#"{"id":"re_fixture","object":"refund","status":"succeeded"}"#,
        r#"{"id":"sub_fixture","object":"subscription","status":"trialing"}"#,
        r#"{"id":"sub_fixture","object":"subscription","status":"canceled"}"#,
        r#"{"id":"sub_fixture","object":"subscription","status":"canceled"}"#,
    ])
    .await;
    let mut adapter = adapter();
    adapter.client = ClientBuilder::new("sk_test_fixture")
        .url(format!("{url}/"))
        .build()
        .unwrap();
    let parent = Private::new(br#"{"id":"pi_fixture","resource":"payment_intents"}"#.to_vec());
    assert_eq!(
        adapter
            .execute(
                &request("stripe.refund.create", serde_json::json!({"amount":400})),
                "refund",
                Some(&parent)
            )
            .await
            .unwrap()
            .state,
        State::Refunded
    );
    let active = adapter
        .execute(
            &request(
                "stripe.subscription.create",
                serde_json::json!({"customer":"cus_fixture"}),
            ),
            "recurring",
            None,
        )
        .await
        .unwrap();
    assert_eq!(active.state, State::Active);
    assert_eq!(
        adapter
            .execute(
                &request("stripe.subscription.cancel", serde_json::json!({})),
                "cancel",
                active.continuation.as_ref()
            )
            .await
            .unwrap()
            .state,
        State::Cancelled
    );
    assert_eq!(
        adapter
            .execute(
                &request(
                    "stripe.subscription.cancel",
                    serde_json::json!({"prorate":true})
                ),
                "cancel-with-terms",
                active.continuation.as_ref()
            )
            .await
            .unwrap()
            .state,
        State::Cancelled
    );
    let calls = calls.await.unwrap();
    assert!(calls[0].contains("payment_intent=pi_fixture"));
    assert!(calls[2].starts_with("DELETE /v1/subscriptions/sub_fixture "));
    assert!(calls[3].starts_with("DELETE /v1/subscriptions/sub_fixture?prorate=true "));
}

#[tokio::test]
async fn malformed_or_lost_provider_replies_stay_unknown() {
    let malformed = [
        "",
        "{}",
        r#"{"id":"pi_fixture"}"#,
        r#"{"id":"pi_fixture","object":"payment_intent"}"#,
        r#"{"id":null,"object":"payment_intent","status":"succeeded"}"#,
        r#"{"id":"pi_fixture","object":"payment_intent","status":true}"#,
        r#"{"id":"pi_fixture","object":"payment_intent","status":"succeeded","client_secret":false}"#,
        r#"{"id":"../wrong","object":"payment_intent","status":"succeeded"}"#,
    ];
    let mut replies = malformed.to_vec();
    replies.push("");
    let (url, calls) = server(replies).await;
    let mut adapter = adapter();
    adapter.client = ClientBuilder::new("sk_test_fixture")
        .url(format!("{url}/"))
        .build()
        .unwrap();
    for _ in malformed {
        assert_eq!(
            adapter
                .execute(
                    &request("stripe.intent.create", serde_json::json!({})),
                    "key",
                    None
                )
                .await
                .unwrap()
                .state,
            State::Unknown
        );
    }
    let reference = Private::new(br#"{"id":"pi_fixture","resource":"payment_intents"}"#.to_vec());
    let observed = adapter.observe("anything", Some(&reference)).await.unwrap();
    assert_eq!(observed.state, State::Unknown);
    assert!(observed.continuation.is_some());
    assert_eq!(
        adapter.observe("anything", None).await.unwrap().state,
        State::Unknown
    );
    calls.await.unwrap();
}

#[test]
fn provider_data_is_reduced_and_unrecognized_statuses_never_prove_payment() {
    for (resource, object, status, state) in [
        (
            "payment_intents",
            "payment_intent",
            "canceled",
            State::Released,
        ),
        (
            "payment_intents",
            "payment_intent",
            "requires_action",
            State::RequiresAction,
        ),
        (
            "payment_intents",
            "payment_intent",
            "processing",
            State::Pending,
        ),
        ("refunds", "refund", "failed", State::Failed),
        (
            "subscriptions",
            "subscription",
            "incomplete_expired",
            State::Failed,
        ),
        (
            "subscriptions",
            "subscription",
            "new-status",
            State::Unknown,
        ),
    ] {
        let reply = Reply {
            amount: None,
            amount_received: None,
            amount_capturable: None,
            currency: None,
            id: "id_fixture".into(),
            object: object.into(),
            status: status.into(),
            client_secret: Some("secret".into()),
            next_action: Some(serde_json::json!({"url":"https://example.test"})),
        };
        let observation = Stripe::reduce(reply, resource, None).unwrap();
        assert_eq!(observation.state, state);
        assert_eq!(
            observation.member_action.is_some(),
            state == State::RequiresAction
        );
    }
    for (resource, expected) in [
        ("bad", None),
        ("refunds", None),
        ("payment_intents", Some("other")),
    ] {
        assert!(Stripe::reduce(serde_json::from_str(PAID).unwrap(), resource, expected).is_err());
    }
    let mut reply: Reply = serde_json::from_str(PAID).unwrap();
    reply.id = "../escape".into();
    assert!(Stripe::reduce(reply, "payment_intents", None).is_err());
}

#[tokio::test]
async fn member_input_cannot_change_terms_or_choose_another_members_object() {
    let adapter = adapter();
    let mut req = request("stripe.intent.create", serde_json::json!({"amount":100}));
    req.evidence = Private::new(br#""pm_fixture""#.to_vec());
    assert_eq!(
        adapter.parameters(&req).unwrap()["payment_method"],
        "pm_fixture"
    );
    req.operation = "stripe.subscription.create".into();
    assert_eq!(
        adapter.parameters(&req).unwrap()["default_payment_method"],
        "pm_fixture"
    );
    for params in [
        serde_json::json!({"transfer_data[destination]":"acct_other"}),
        serde_json::json!({"bad\nkey":"value"}),
        serde_json::json!({"charge":"ch_other"}),
        serde_json::json!({"payment_intent":"pi_other"}),
        serde_json::json!({"transfer_data":{"destination":"acct_other"}}),
        serde_json::json!({"nested":[{"on_behalf_of":"acct_other"}]}),
        serde_json::json!({"payment_method":"pm_other"}),
    ] {
        req = request("stripe.intent.create", params);
        req.evidence = Private::new(br#""pm_fixture""#.to_vec());
        assert!(adapter.parameters(&req).is_err());
    }
    req = request("stripe.intent.create", serde_json::json!({}));
    req.evidence = Private::new(b"{}".to_vec());
    assert!(adapter.parameters(&req).is_err());
    req.evidence = Private::new(br#""../bad""#.to_vec());
    assert!(adapter.parameters(&req).is_err());
    assert!(
        adapter
            .execute(
                &request("stripe.intent.capture", serde_json::json!({})),
                "key",
                None
            )
            .await
            .is_err()
    );
    assert!(
        adapter
            .execute(&request("unsupported", serde_json::json!({})), "key", None)
            .await
            .is_err()
    );
    for bytes in [
        b"bad".as_slice(),
        br#"{"id":"../x","resource":"payment_intents"}"#,
        br#"{"id":"pi_x","resource":"transfers"}"#,
    ] {
        assert!(reference(&Private::new(bytes.to_vec())).is_err());
    }
    for id in ["", "../path", &"a".repeat(257)] {
        assert!(valid_id(id).is_err());
    }
    let mut limited = self::adapter();
    limited.max_input_bytes = 1;
    assert!(
        limited
            .parameters(&request("stripe.intent.create", serde_json::json!({})))
            .is_err()
    );
    assert!(
        adapter
            .parameters(&request("stripe.intent.create", Value::Null))
            .is_err()
    );
}

#[test]
fn configuration_requires_explicit_credentials_and_limits() {
    for (id, secret, timeout, max) in [
        ("", vec![1], 1, 1),
        ("stripe", vec![255], 1, 1),
        ("stripe", vec![], 1, 1),
        ("stripe", vec![1], 0, 1),
        ("stripe", vec![1], 1, 0),
    ] {
        assert!(
            configured(
                id.into(),
                Private::new(secret),
                Duration::from_secs(timeout),
                max
            )
            .is_err()
        );
    }
}

#[tokio::test]
async fn lost_known_object_replies_preserve_read_only_reconciliation() {
    let (url, calls) = server(vec!["", PAID, ""]).await;
    let mut adapter = adapter();
    adapter.client = ClientBuilder::new("sk_test_fixture")
        .url(format!("{url}/"))
        .build()
        .unwrap();
    let parent = Private::new(br#"{"id":"pi_fixture","resource":"payment_intents"}"#.to_vec());
    let result = adapter
        .execute(
            &request("stripe.intent.capture", serde_json::json!({})),
            "capture",
            Some(&parent),
        )
        .await
        .unwrap();
    assert_eq!(result.state, State::Unknown);
    assert_eq!(
        adapter
            .observe("stripe.intent.capture", result.continuation.as_ref())
            .await
            .unwrap()
            .state,
        State::Collected
    );
    let refund = adapter
        .execute(
            &request("stripe.refund.create", serde_json::json!({})),
            "refund",
            Some(&parent),
        )
        .await
        .unwrap();
    assert_eq!(refund.state, State::Unknown);
    assert!(refund.continuation.is_none());
    let calls = calls.await.unwrap();
    assert_eq!(calls.len(), 3);
    assert!(calls[1].starts_with("GET /v1/payment_intents/pi_fixture "));
}

#[tokio::test]
async fn invalid_sdk_credentials_and_idempotency_keys_fail_before_network() {
    assert!(
        configured(
            "stripe".into(),
            Private::new(b"invalid\nsecret".to_vec()),
            Duration::from_secs(1),
            100
        )
        .is_err()
    );
    assert!(matches!(
        adapter()
            .execute(
                &request("stripe.intent.create", serde_json::json!({})),
                "",
                None
            )
            .await,
        Err(Error::Invalid)
    ));
}

#[derive(Clone)]
struct TraceRecorder(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl tracing::Subscriber for TraceRecorder {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, _: &tracing::Event<'_>) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[tokio::test]
async fn host_tracing_cannot_capture_sdk_payment_paths_or_keys() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let count = Arc::new(AtomicUsize::new(0));
    let recorder = TraceRecorder(count.clone());
    let (url, calls) = server(vec![PAID, PAID]).await;
    let mut adapter = adapter();
    adapter.client = ClientBuilder::new("sk_test_fixture")
        .url(format!("{url}/"))
        .build()
        .unwrap();
    async {
        tracing::info!("outside payment boundary");
        assert!(count.swap(0, Ordering::SeqCst) > 0);
        let result = adapter
            .execute(
                &request("stripe.intent.create", serde_json::json!({})),
                "private-operation-key",
                None,
            )
            .await
            .unwrap();
        assert_eq!(result.state, State::Collected);
        assert_eq!(
            adapter
                .observe("stripe.intent.create", result.continuation.as_ref())
                .await
                .unwrap()
                .state,
            State::Collected
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
    .with_subscriber(recorder)
    .await;
    assert_eq!(calls.await.unwrap().len(), 2);
}

#[tokio::test]
async fn deeply_nested_provider_actions_cannot_panic_or_prove_payment() {
    let nested = format!("{}null{}", "[".repeat(160), "]".repeat(160));
    let body = format!(
        r#"{{"id":"pi_fixture","object":"payment_intent","status":"requires_action","next_action":{nested}}}"#
    );
    let (url, calls) = server(vec![&body]).await;
    let mut adapter = adapter();
    adapter.client = ClientBuilder::new("sk_test_fixture")
        .url(format!("{url}/"))
        .build()
        .unwrap();
    let result = adapter
        .execute(
            &request("stripe.intent.create", serde_json::json!({})),
            "key",
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.state, State::Unknown);
    assert!(result.member_action.is_none());
    assert_eq!(calls.await.unwrap().len(), 1);
}

#[tokio::test]
async fn member_actions_are_projected_without_unneeded_provider_details() {
    let nested = format!("{}null{}", "[".repeat(160), "]".repeat(160));
    let body = format!(
        r#"{{"id":"pi_fixture","object":"payment_intent","status":"requires_action","client_secret":"member-secret","next_action":{{"type":"fixture"}},"billing_details":{nested},"customer":"discard-this"}}"#
    );
    let (url, calls) = server(vec![&body]).await;
    let mut adapter = adapter();
    adapter.client = ClientBuilder::new("sk_test_fixture")
        .url(format!("{url}/"))
        .build()
        .unwrap();
    let result = adapter
        .execute(
            &request("stripe.intent.create", serde_json::json!({})),
            "key",
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.state, State::RequiresAction);
    assert_eq!(
        serde_json::from_slice::<Value>(result.member_action.unwrap().expose()).unwrap(),
        serde_json::json!({"client_secret":"member-secret","next_action":{"type":"fixture"}})
    );
    assert_eq!(
        serde_json::from_slice::<Value>(result.continuation.unwrap().expose()).unwrap(),
        serde_json::json!({"id":"pi_fixture","resource":"payment_intents"})
    );
    assert_eq!(calls.await.unwrap().len(), 1);
}

fn configured(id: String, secret: Private, timeout: Duration, max: usize) -> Result<Stripe> {
    Stripe::new(id, secret, timeout, max, Mode::Sandbox)
}
impl Stripe {
    async fn execute(
        &self,
        request: &Instruction,
        key: &str,
        previous: Option<&Private>,
    ) -> Result<Observation> {
        let auth = Dispatch::new(
            request,
            crate::test_support::permit(request, self.mode),
            0,
            std::sync::Arc::new(|| 0),
        )?;
        Adapter::execute(self, &auth, key, previous).await
    }
    async fn observe(&self, operation: &str, previous: Option<&Private>) -> Result<Observation> {
        let mut request = request(operation, serde_json::json!({}));
        request.kind = Action::Observe;
        let auth = Dispatch::new(
            &request,
            crate::test_support::permit(&request, self.mode),
            0,
            std::sync::Arc::new(|| 0),
        )?;
        Adapter::observe(self, &auth, previous).await
    }
}

#[tokio::test]
async fn financial_projection_and_complete_private_sdk_results_use_real_response_fields() {
    let body = r#"{"id":"pi_fixture","object":"payment_intent","status":"succeeded","amount":1200,"amount_received":1200,"currency":"chf","billing_details":{"name":"private fixture"},"extension":{"unrecognized":true}}"#;
    let error = r#"{"error":{"type":"card_error","message":"private issuer detail","charge":"ch_fixture"}}"#;
    let (url, calls) = server_status(vec![
        (200, body.into()),
        (402, error.into()),
        (402, error.into()),
    ])
    .await;
    let adapter = Stripe::from_client(
        "stripe".into(),
        ClientBuilder::new("sk_test_fixture")
            .url(format!("{url}/"))
            .build()
            .unwrap(),
        Duration::from_secs(2),
        8192,
        Mode::Sandbox,
    )
    .unwrap();
    let request = request(
        "stripe.intent.create",
        serde_json::json!({"amount":1200,"currency":"CHF"}),
    );
    let result = adapter.execute(&request, "create", None).await.unwrap();
    assert!(result.satisfies_terms);
    let full: Value = serde_json::from_slice(result.upstream[0].payload.expose()).unwrap();
    assert_eq!(full["extension"]["unrecognized"], true);
    assert!(
        String::from_utf8_lossy(result.continuation.as_ref().unwrap().expose()).contains("1200")
    );
    assert!(
        !String::from_utf8_lossy(result.continuation.as_ref().unwrap().expose())
            .contains("private fixture")
    );
    let failed = adapter.execute(&request, "failed", None).await.unwrap();
    assert_eq!(failed.state, State::Unknown);
    assert_eq!(failed.upstream[0].http_status, Some(402));
    assert!(
        String::from_utf8_lossy(failed.upstream[0].payload.expose())
            .contains("private issuer detail")
    );
    let read = adapter
        .observe("stripe.intent.create", result.continuation.as_ref())
        .await
        .unwrap();
    assert_eq!(read.state, State::Unknown);
    assert!(read.continuation.is_some());
    assert_eq!(read.upstream.len(), 1);
    assert_eq!(calls.await.unwrap().len(), 3);
    let financial = Financial {
        amount: 1200,
        currency: "chf".into(),
    };
    for (state, amount, received, capturable, expected) in [
        (State::Collected, Some(1200), Some(1200), None, true),
        (State::Collected, Some(1200), Some(100), None, false),
        (State::Authorized, None, None, Some(1200), true),
        (State::Refunded, Some(1200), None, None, true),
        (State::Released, Some(1200), None, None, true),
        (State::Pending, Some(1200), None, None, false),
    ] {
        assert_eq!(
            financial.satisfied(amount, received, capturable, Some("CHF"), state),
            expected
        );
    }
    assert!(!financial.satisfied(Some(1200), Some(1200), None, None, State::Collected));
    assert!(!financial.satisfied(Some(1200), Some(1200), None, Some("usd"), State::Collected));
    for terms in [
        serde_json::json!({"amount":"bad"}),
        serde_json::json!({"currency":3}),
    ] {
        assert!(Financial::from_terms(terms.as_object().unwrap(), Some(&financial)).is_none());
    }
    assert_eq!(
        Financial::from_terms(
            serde_json::json!({"amount_to_capture":"500"})
                .as_object()
                .unwrap(),
            Some(&financial)
        )
        .unwrap()
        .amount,
        500
    );
}

#[tokio::test]
async fn actual_mode_action_and_timeout_edges_cannot_mint_success() {
    assert!(
        Stripe::new(
            "stripe".into(),
            Private::new(b"sk_test_fixture".to_vec()),
            Duration::from_secs(1),
            8192,
            Mode::Simulation
        )
        .is_err()
    );
    for (timeout, max, mode) in [
        (Duration::ZERO, 8192, Mode::Production),
        (Duration::from_secs(1), 0, Mode::Production),
        (Duration::from_secs(1), 8192, Mode::Simulation),
    ] {
        assert!(
            Stripe::from_client(
                "stripe".into(),
                ClientBuilder::new("sk_test_fixture").build().unwrap(),
                timeout,
                max,
                mode
            )
            .is_err()
        );
    }
    let adapter = adapter();
    assert_eq!(Adapter::mode(&adapter), Mode::Sandbox);
    let mut instruction = request("stripe.intent.create", serde_json::json!({}));
    instruction.kind = Action::Observe;
    let dispatch = Dispatch::new(
        &instruction,
        crate::test_support::permit(&instruction, Mode::Sandbox),
        0,
        std::sync::Arc::new(|| 0),
    )
    .unwrap();
    assert!(
        Adapter::execute(&adapter, &dispatch, "key", None)
            .await
            .is_err()
    );
    instruction.kind = Action::Execute;
    let dispatch = Dispatch::new(
        &instruction,
        crate::test_support::permit(&instruction, Mode::Sandbox),
        0,
        std::sync::Arc::new(|| 0),
    )
    .unwrap();
    assert!(Adapter::observe(&adapter, &dispatch, None).await.is_err());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let stall = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.unwrap();
            assert!(!bytes.is_empty());
        }
    });
    let adapter = Stripe::from_client(
        "stripe".into(),
        ClientBuilder::new("sk_test_fixture")
            .url(url)
            .build()
            .unwrap(),
        Duration::from_millis(100),
        8192,
        Mode::Sandbox,
    )
    .unwrap();
    assert_eq!(
        adapter
            .execute(&instruction, "key", None)
            .await
            .unwrap()
            .state,
        State::Unknown
    );
    let previous = Private::new(br#"{"resource":"payment_intents","id":"pi_fixture"}"#.to_vec());
    let observed = adapter
        .observe("stripe.intent.create", Some(&previous))
        .await
        .unwrap();
    assert_eq!(observed.state, State::Unknown);
    assert!(observed.continuation.is_some());
    stall.await.unwrap();
    let (url, calls) = server(vec!["{}"]).await;
    let adapter = Stripe::from_client(
        "stripe".into(),
        ClientBuilder::new("sk_test_fixture")
            .url(format!("{url}/"))
            .build()
            .unwrap(),
        Duration::from_secs(1),
        8192,
        Mode::Sandbox,
    )
    .unwrap();
    assert!(
        adapter
            .observe("stripe.intent.create", Some(&previous))
            .await
            .unwrap()
            .continuation
            .is_some()
    );
    assert_eq!(calls.await.unwrap().len(), 1);
}

#[tokio::test]
async fn injected_client_retry_policy_cannot_repeat_a_costed_observation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut count = 0;
        while let Ok(Ok((mut stream, _))) =
            tokio::time::timeout(Duration::from_millis(200), listener.accept()).await
        {
            let mut buf = [0; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            assert!(String::from_utf8_lossy(&buf[..n]).starts_with("GET "));
            let body = r#"{"error":{"type":"api_error","message":"fixture"}}"#;
            stream.write_all(format!("HTTP/1.1 500 Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            count += 1;
        }
        count
    });
    let adapter = Stripe::from_client(
        "stripe".into(),
        ClientBuilder::new("sk_test_fixture")
            .url(url)
            .request_strategy(RequestStrategy::Retry(3))
            .build()
            .unwrap(),
        Duration::from_secs(2),
        8192,
        Mode::Sandbox,
    )
    .unwrap();
    let previous = Private::new(br#"{"resource":"payment_intents","id":"pi_fixture"}"#.to_vec());
    assert_eq!(
        adapter
            .observe("stripe.intent.create", Some(&previous))
            .await
            .unwrap()
            .state,
        State::Unknown
    );
    assert_eq!(server.await.unwrap(), 1);
}
