use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn server(replies: Vec<&'static str>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for reply in replies {
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
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",reply.len(),reply).as_bytes()).await.unwrap();
            }
        }
        requests
    });
    (url, task)
}

fn adapter() -> Stripe {
    Stripe::new(
        "stripe".into(),
        Private::new(b"sk_test_fixture".to_vec()),
        Duration::from_secs(2),
        8192,
    )
    .unwrap()
}
fn request(op: &str, terms: Value) -> Instruction {
    Instruction {
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
    adapter.client = Client::from_url(url.as_str(), "sk_test_fixture");
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
    assert!(calls[2].starts_with("GET /v1/payment_intents/pi_fixture "));
}

#[tokio::test]
async fn refunds_and_recurring_arrangements_report_distinct_facts() {
    let (url, calls) = server(vec![
        r#"{"id":"re_fixture","object":"refund","status":"succeeded"}"#,
        r#"{"id":"sub_fixture","object":"subscription","status":"trialing"}"#,
        r#"{"id":"sub_fixture","object":"subscription","status":"canceled"}"#,
    ])
    .await;
    let mut adapter = adapter();
    adapter.client = Client::from_url(url.as_str(), "sk_test_fixture");
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
    let calls = calls.await.unwrap();
    assert!(calls[0].contains("payment_intent=pi_fixture"));
    assert!(calls[2].starts_with("DELETE "));
    assert_eq!(
        calls[2]
            .split_whitespace()
            .nth(1)
            .unwrap()
            .trim_end_matches('?'),
        "/v1/subscriptions/sub_fixture"
    );
}

#[tokio::test]
async fn malformed_or_lost_provider_replies_stay_unknown() {
    let (url, calls) = server(vec![
        "",
        "{}",
        r#"{"id":"../wrong","object":"payment_intent","status":"succeeded"}"#,
        "",
    ])
    .await;
    let mut adapter = adapter();
    adapter.client = Client::from_url(url.as_str(), "sk_test_fixture");
    for _ in 0..3 {
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
            Stripe::new(
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
    adapter.client = Client::from_url(url.as_str(), "sk_test_fixture");
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
