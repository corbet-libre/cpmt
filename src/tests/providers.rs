use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
fn limits() -> Limits {
    Limits {
        timeout: Duration::from_secs(2),
        input_bytes: 8192,
        response_bytes: 8192,
    }
}
fn providers() -> Vec<Provider> {
    vec![
        Provider::Mollie { test_mode: None },
        Provider::GoCardless { sandbox: false },
        Provider::Square {
            sandbox: false,
            version: "2026-09-16".into(),
        },
        Provider::Btcpay {
            base: "https://btcpay.example/".into(),
            store: "store".into(),
        },
        Provider::CoinGate { sandbox: false },
    ]
}
fn adapter(provider: Provider) -> Rest {
    configured(
        "fixture".into(),
        provider,
        Private::new(b"fixture-secret".to_vec()),
        limits(),
    )
    .unwrap()
}
fn amount() -> Amount {
    Amount {
        value: "10".into(),
        currency: "CHF".into(),
    }
}
fn reference(resource: &str) -> Reference {
    Reference {
        resource: resource.into(),
        id: "id_123".into(),
        parent: None,
        amount: amount(),
    }
}
fn terms(provider: &Provider) -> Value {
    match provider {
        Provider::Mollie { .. } => {
            json!({"amount":{"value":"10.00","currency":"CHF"},"description":"fixture"})
        }
        Provider::Square { .. } => {
            json!({"amount_money":{"amount":10,"currency":"CHF"},"source_id":"fixture-token"})
        }
        Provider::CoinGate { .. } => json!({"price_amount":10,"price_currency":"CHF"}),
        _ => json!({"amount":"10","currency":"CHF"}),
    }
}
fn object(provider: &Provider, status: &str) -> Value {
    let mut value = terms(provider);
    let map = value.as_object_mut().unwrap();
    map.insert("id".into(), json!("id_123"));
    map.insert("status".into(), json!(status));
    map.insert("source_type".into(), json!("CARD"));
    map.insert("retry_if_possible".into(), json!(false));
    map.insert(
        "approved_money".into(),
        json!({"amount":10,"currency":"CHF"}),
    );
    map.insert("storeId".into(), json!("store"));
    map.insert("additionalStatus".into(), json!("None"));
    map.insert("paidAmount".into(), json!("10"));
    map.insert(
        "billing_details".into(),
        json!({"name":"NEVER RETAIN", "email":"private@example.test"}),
    );
    value
}
fn wrap(provider: &Provider, resource: &str, value: Value) -> Value {
    match provider {
        Provider::Square { .. } => {
            json!({if resource == "refunds" {"refund"} else {"payment"}:value})
        }
        Provider::GoCardless { .. } => json!({resource:value}),
        _ => value,
    }
}
fn instruction(operation: &str, terms: Value) -> Instruction {
    Instruction {
        kind: Action::Execute,
        id: [1; 32],
        parent: None,
        adapter: "fixture".into(),
        operation: operation.into(),
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
async fn server(replies: Vec<String>) -> (Url, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        let mut calls = vec![];
        for reply in replies {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = vec![];
            let mut buffer = [0; 4096];
            loop {
                let n = stream.read(&mut buffer).await.unwrap();
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..n]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                    let size = headers
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: "))
                        .map_or(0, |n| n.parse::<usize>().unwrap());
                    if bytes.len() >= end + 4 + size {
                        break;
                    }
                }
            }
            calls.push(String::from_utf8(bytes).unwrap());
            if !reply.is_empty() {
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        }
        calls
    });
    (base, task)
}
fn http(value: Value) -> String {
    let body = value.to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}
fn local(provider: Provider, base: Url) -> Rest {
    build(
        "fixture".into(),
        provider,
        Private::new(b"fixture-secret".to_vec()),
        limits(),
        Some(base),
    )
    .unwrap()
}

#[tokio::test]
async fn every_provider_collects_and_observes_using_authenticated_minimal_references() {
    for (provider, status, resource, path, auth) in providers()
        .into_iter()
        .zip([
            ("paid", "payments", "payments", "Bearer"),
            ("confirmed", "payments", "payments", "Bearer"),
            ("COMPLETED", "payments", "payments", "Bearer"),
            (
                "Settled",
                "invoices",
                "api/v1/stores/store/invoices",
                "token",
            ),
            ("paid", "orders", "orders", "Token"),
        ])
        .map(|(p, (s, r, path, a))| (p, s, r, path, a))
    {
        let value = object(&provider, status);
        let reply = wrap(&provider, resource, value);
        let (base, calls) = server(vec![http(reply.clone()), http(reply)]).await;
        let adapter = local(provider, base);
        assert_eq!(adapter.max_input_bytes(), 8192);
        let operation = adapter.capabilities().operations[0].clone();
        let result = adapter
            .execute(
                &instruction(&operation, terms(&adapter.provider)),
                "same-operation-key",
                None,
            )
            .await
            .unwrap();
        assert!(matches!(result.state, State::Collected | State::Settled));
        let headers: Vec<(String, Vec<u8>)> =
            serde_json::from_slice(result.upstream[0].headers.as_ref().unwrap().expose()).unwrap();
        assert!(headers.iter().any(|(name, _)| name == "content-length"));
        let retained = result.continuation.as_ref().unwrap();
        assert!(!String::from_utf8_lossy(retained.expose()).contains("NEVER"));
        assert!(
            !String::from_utf8_lossy(result.claim.as_ref().unwrap().expose()).contains("private@")
        );
        let observed = adapter.observe(&operation, Some(retained)).await.unwrap();
        assert_eq!(observed.state, result.state);
        assert_eq!(
            observed.claim.unwrap().expose(),
            result.claim.unwrap().expose()
        );
        let calls = calls.await.unwrap();
        assert!(calls[0].starts_with(&format!("POST /{path} ")));
        assert!(calls[0].contains(&format!("authorization: {auth} fixture-secret")));
        assert!(calls[1].starts_with(&format!(
            "GET /{}id_123 ",
            if resource == "invoices" {
                "api/v1/invoices/"
            } else if resource == "orders" {
                "orders/"
            } else {
                "payments/"
            }
        )));
        assert_eq!(calls[1].split_once("\r\n\r\n").unwrap().1, "");
        if matches!(adapter.provider, Provider::Square { .. }) {
            let body: Value =
                serde_json::from_str(calls[0].split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body["idempotency_key"].as_str().unwrap().len(), 43);
            assert!(calls[0].contains("square-version: 2026-09-16"));
        }
    }
}
#[tokio::test]
async fn children_use_parent_ids_and_do_not_substitute_parent_for_lost_refund() {
    for provider in providers().into_iter().take(3) {
        let refund_status = match provider {
            Provider::Mollie { .. } => "refunded",
            Provider::Square { .. } => "COMPLETED",
            _ => "paid",
        };
        let mut value = object(&provider, refund_status);
        value["paymentId"] = json!("id_123");
        value["payment_id"] = json!("id_123");
        value["links"] = json!({"payment":"id_123"});
        value["id"] = json!("refund_1");
        let reply = wrap(&provider, "refunds", value);
        let (base, calls) = server(vec![
            http(reply.clone()),
            http(reply),
            String::new(),
            String::new(),
        ])
        .await;
        let adapter = local(provider, base);
        let mut refund_terms = terms(&adapter.provider);
        if matches!(adapter.provider, Provider::GoCardless { .. }) {
            refund_terms = json!({"amount":10,"total_amount_confirmation":10});
        }
        let request = instruction(
            &format!("{}.refund.create", adapter.provider.name()),
            refund_terms,
        );
        let parent = encode(&reference("payments"));
        let refunded = adapter
            .execute(&request, "refund-key", Some(&parent))
            .await
            .unwrap();
        assert_eq!(refunded.state, State::Refunded);
        assert_eq!(
            adapter
                .observe(&request.operation, refunded.continuation.as_ref())
                .await
                .unwrap()
                .state,
            State::Refunded
        );
        let missing = adapter
            .execute(&request, "different-refund-key", Some(&parent))
            .await
            .unwrap();
        assert_eq!(missing.state, State::Unknown);
        assert!(missing.continuation.is_none());
        let cancelled = adapter
            .execute(
                &instruction(
                    &format!("{}.payment.cancel", adapter.provider.name()),
                    json!({}),
                ),
                "cancel-key",
                Some(&parent),
            )
            .await
            .unwrap();
        assert_eq!(cancelled.state, State::Unknown);
        assert!(cancelled.continuation.is_some());
        let calls = calls.await.unwrap();
        let path = if matches!(adapter.provider, Provider::Mollie { .. }) {
            "payments/id_123/refunds"
        } else {
            "refunds"
        };
        assert!(calls[0].starts_with(&format!("POST /{path} ")));
        assert!(calls[1].starts_with(&format!("GET /{path}/refund_1 ")));
    }
}
#[test]
fn provider_states_do_not_confuse_partial_recorded_or_manual_payments_with_collection() {
    let cases = [
        vec![
            ("paid", State::Collected),
            ("authorized", State::Authorized),
            ("open", State::RequiresAction),
            ("pending", State::Pending),
            ("queued", State::Pending),
            ("processing", State::Pending),
            ("canceled", State::Cancelled),
            ("expired", State::Failed),
            ("failed", State::Failed),
            ("future", State::Unknown),
        ],
        vec![
            ("confirmed", State::Collected),
            ("paid_out", State::Collected),
            ("cancelled", State::Cancelled),
            ("pending_customer_approval", State::RequiresAction),
            ("pending_submission", State::Pending),
            ("submitted", State::Pending),
            ("pending", State::Pending),
            ("created", State::Pending),
            ("failed", State::Failed),
            ("customer_approval_denied", State::Failed),
            ("bounced", State::Failed),
            ("funds_returned", State::Failed),
            ("charged_back", State::Unknown),
        ],
        vec![
            ("COMPLETED", State::Collected),
            ("APPROVED", State::Authorized),
            ("CANCELED", State::Cancelled),
            ("PENDING", State::Pending),
            ("FAILED", State::Failed),
            ("REJECTED", State::Failed),
            ("future", State::Unknown),
        ],
        vec![
            ("New", State::RequiresAction),
            ("Processing", State::Pending),
            ("Settled", State::Settled),
            ("Expired", State::Unknown),
            ("Invalid", State::Unknown),
            ("future", State::Unknown),
        ],
        vec![
            ("new", State::RequiresAction),
            ("pending", State::RequiresAction),
            ("confirming", State::Pending),
            ("paid", State::Settled),
            ("refunded", State::Refunded),
            ("expired", State::Unknown),
            ("invalid", State::Unknown),
            ("canceled", State::Unknown),
            ("future", State::Unknown),
        ],
    ];
    for (provider, cases) in providers().into_iter().zip(cases) {
        for (status, state) in cases {
            assert_eq!(
                provider
                    .state(&object(&provider, status), "payments", &amount())
                    .unwrap(),
                state
            );
        }
        assert!(provider.state(&json!({}), "payments", &amount()).is_err());
    }
    for (provider, status) in providers()
        .into_iter()
        .take(3)
        .zip(["refunded", "paid", "COMPLETED"])
    {
        assert_eq!(
            provider
                .state(&object(&provider, status), "refunds", &amount())
                .unwrap(),
            State::Refunded
        );
    }
    let mollie = Provider::Mollie { test_mode: None };
    let mut paid = object(&mollie, "paid");
    paid["amountCaptured"] = json!({"value":"10.0","currency":"CHF"});
    assert_eq!(
        mollie.state(&paid, "payments", &amount()).unwrap(),
        State::Collected
    );
    paid["amountCaptured"]["value"] = json!("5");
    assert!(mollie.state(&paid, "payments", &amount()).is_err());
    paid.as_object_mut().unwrap().remove("amountCaptured");
    paid["captureMode"] = json!("manual");
    assert!(mollie.state(&paid, "payments", &amount()).is_err());
    let square = providers().remove(2);
    let mut paid = object(&square, "COMPLETED");
    for source in ["CASH", "EXTERNAL", "future"] {
        paid["source_type"] = json!(source);
        assert!(square.state(&paid, "payments", &amount()).is_err());
    }
    paid["source_type"] = json!("CARD");
    paid["amount_money"]["amount"] = json!(5);
    assert!(square.state(&paid, "payments", &amount()).is_err());
    paid["amount_money"]["amount"] = json!(10);
    paid["amount_money"]["currency"] = json!("EUR");
    assert!(square.state(&paid, "payments", &amount()).is_err());
    let btc = providers().remove(3);
    let mut paid = object(&btc, "Settled");
    for additional in ["Marked", "PaidPartial", "PaidLate", "future"] {
        paid["additionalStatus"] = json!(additional);
        assert_eq!(
            btc.state(&paid, "invoices", &amount()).unwrap(),
            State::Unknown
        );
    }
    paid["additionalStatus"] = json!("PaidOver");
    paid["paidAmount"] = json!("11");
    assert_eq!(
        btc.state(&paid, "invoices", &amount()).unwrap(),
        State::Settled
    );
    paid["paidAmount"] = json!("9");
    assert!(btc.state(&paid, "invoices", &amount()).is_err());
    paid["paidAmount"] = json!("garbage");
    assert!(btc.state(&paid, "invoices", &amount()).is_err());
}
#[test]
fn constructors_validate_secrets_endpoints_limits_and_api_versions() {
    for provider in providers() {
        let a = adapter(provider);
        assert!(!a.capabilities().operations.is_empty());
    }
    for provider in [
        Provider::Square {
            sandbox: true,
            version: "2026-09-16".into(),
        },
        Provider::CoinGate { sandbox: true },
        Provider::GoCardless { sandbox: true },
    ] {
        assert!(adapter(provider).base.as_str().contains("sandbox"));
    }
    for version in ["", "2026/09/16", "202X-09-16"] {
        assert!(
            Provider::Square {
                sandbox: false,
                version: version.into()
            }
            .endpoint()
            .is_err()
        );
    }
    for base in [
        "garbage",
        "http://example.test",
        "https://user@example.test",
        "https://u:p@example.test",
        "https://:p@example.test",
        "https://example.test/?x=y",
        "https://example.test/#fragment",
    ] {
        assert!(
            configured(
                "id".into(),
                Provider::Btcpay {
                    base: base.into(),
                    store: "store".into()
                },
                Private::new(b"key".to_vec()),
                limits()
            )
            .is_err()
        );
    }
    assert!(
        Provider::Btcpay {
            base: "https://example.test".into(),
            store: "../".into()
        }
        .endpoint()
        .is_err()
    );
    for secret in [vec![], vec![255], b"key\r\nx: leak".to_vec()] {
        assert!(
            configured(
                "id".into(),
                Provider::Mollie { test_mode: None },
                Private::new(secret),
                limits()
            )
            .is_err()
        );
    }
    for limits in [
        Limits {
            timeout: Duration::ZERO,
            ..limits()
        },
        Limits {
            input_bytes: 0,
            ..limits()
        },
        Limits {
            response_bytes: 0,
            ..limits()
        },
    ] {
        assert!(
            configured(
                "id".into(),
                Provider::Mollie { test_mode: None },
                Private::new(b"key".to_vec()),
                limits
            )
            .is_err()
        );
    }
    assert!(
        configured(
            "".into(),
            Provider::Mollie { test_mode: None },
            Private::new(b"key".to_vec()),
            limits()
        )
        .is_err()
    );
    assert!(
        build(
            "id".into(),
            Provider::Mollie { test_mode: None },
            Private::new(b"key".to_vec()),
            limits(),
            Some(Url::parse("ftp://127.0.0.1").unwrap())
        )
        .is_err()
    );
    assert!(
        build(
            "id".into(),
            Provider::Mollie { test_mode: None },
            Private::new(b"key".to_vec()),
            limits(),
            Some(Url::parse("http://outside.example").unwrap())
        )
        .is_err()
    );
}
#[test]
fn input_and_reference_validation_rejects_smuggling_and_wrong_parent() {
    for id in ["", "..", "a/b", "a?x", "é", &"x".repeat(257)] {
        assert!(valid_id(id).is_err());
    }
    valid_id("ABC_123-xy").unwrap();
    for value in [
        json!({}),
        json!({"amount":"x","currency":"CHF"}),
        json!({"amount":0,"currency":"CHF"}),
        json!({"amount":-1,"currency":"CHF"}),
        json!({"amount":1,"currency":""}),
        json!({"amount":1,"currency":"C/H"}),
        json!({"amount":1,"currency":"X".repeat(33)}),
        json!({"amount":true,"currency":"CHF"}),
    ] {
        assert!(Amount::from(&value, "amount", "currency").is_err());
    }
    assert!(decode(&Private::new(vec![])).is_err());
    let mut bad = reference("refunds");
    bad.parent = Some("../".into());
    assert!(decode(&encode(&bad)).is_err());
    bad.parent = Some("valid".into());
    decode(&encode(&bad)).unwrap();
    let p = Provider::Mollie { test_mode: None };
    for op in ["mollie", "other.payment.create", "mollie.transfer"] {
        assert!(p.operation(op).is_err());
    }
    for value in [
        json!({"nested":{"routing":[]}}),
        json!([{"destination":"account"}]),
    ] {
        assert!(forbidden(&value));
    }
    assert!(!forbidden(&json!({"metadata":{"hello":[1,"safe"]}})));
    for provider in providers() {
        let map = terms(&provider).as_object().unwrap().clone();
        assert!(
            provider
                .prepare(
                    provider.operations()[0],
                    map.clone(),
                    "key",
                    Some(&reference("payments"))
                )
                .is_err()
        );
        assert!(
            provider
                .prepare("refund.create", map.clone(), "key", None)
                .is_err()
        );
        assert!(
            provider
                .prepare(
                    "refund.create",
                    map.clone(),
                    "key",
                    Some(&reference("refunds"))
                )
                .is_err()
        );
        for field in ["id", "idempotency_key", "payment_id"] {
            let mut map = map.clone();
            map.insert(field.into(), json!("injected"));
            assert!(
                provider
                    .prepare(provider.operations()[0], map, "key", None)
                    .is_err()
            );
        }
        assert!(provider.read(&reference("alien")).is_err());
        let mut map = Map::new();
        assert!(
            provider
                .member_input("refund.create", &mut map, b"\"token\"")
                .is_err()
        );
    }
    for provider in [Provider::Mollie { test_mode: None }, providers().remove(2)] {
        let mut map = Map::new();
        provider
            .member_input("payment.create", &mut map, b"\"token\"")
            .unwrap();
        assert!(
            provider
                .member_input("payment.create", &mut map, b"\"token\"")
                .is_err()
        );
        assert!(
            provider
                .member_input("payment.create", &mut Map::new(), b"\"\"")
                .is_err()
        );
        assert!(
            provider
                .member_input("payment.create", &mut Map::new(), b"bad")
                .is_err()
        );
    }
    let square = providers().remove(2);
    for source in [Value::Null, json!("CASH"), json!("EXTERNAL")] {
        let mut t = terms(&square);
        t["source_id"] = source;
        assert!(
            square
                .prepare(
                    "payment.create",
                    t.as_object().unwrap().clone(),
                    "key",
                    None
                )
                .is_err()
        );
    }
    let parent = reference("payments");
    let capture = square
        .prepare("payment.capture", Map::new(), "key", Some(&parent))
        .unwrap();
    assert_eq!(capture.path, "payments/id_123/complete");
    assert!(p.read(&reference("refunds")).is_err());
    let gc = Provider::GoCardless { sandbox: true };
    for t in [
        json!({"amount":1,"currency":"CHF"}),
        json!({"amount":1,"links":true}),
        json!({"amount":1,"links":{"payment":"injected"}}),
    ] {
        assert!(
            gc.prepare(
                "refund.create",
                t.as_object().unwrap().clone(),
                "key",
                Some(&parent)
            )
            .is_err()
        );
    }
    assert!(
        p.prepare(
            "unsupported",
            terms(&p).as_object().unwrap().clone(),
            "key",
            None
        )
        .is_err()
    );
}
#[test]
fn reduction_checks_identity_parent_account_and_discards_private_fields() {
    for provider in providers() {
        let a = adapter(provider);
        let prep = a
            .provider
            .prepare(
                a.provider.operations()[0],
                terms(&a.provider).as_object().unwrap().clone(),
                "key",
                None,
            )
            .unwrap();
        assert!(a.reduce(json!([]), &prep).is_err());
        assert!(
            a.reduce(
                wrap(&a.provider, prep.resource, json!({"id":[],"status":"paid"})),
                &prep
            )
            .is_err()
        );
        let mut v = object(&a.provider, "pending");
        v["id"] = json!("../");
        assert!(
            a.reduce(wrap(&a.provider, prep.resource, v), &prep)
                .is_err()
        );
        let mut prepared = prep;
        prepared.expected = Some("different".into());
        assert!(
            a.reduce(
                wrap(
                    &a.provider,
                    prepared.resource,
                    object(&a.provider, "pending")
                ),
                &prepared
            )
            .is_err()
        );
    }
    let btc = adapter(providers().remove(3));
    let prep = btc.provider.read(&reference("invoices")).unwrap();
    let mut value = object(&btc.provider, "New");
    value["storeId"] = json!("other");
    assert!(btc.reduce(value, &prep).is_err());
    for provider in providers().into_iter().take(3) {
        let a = adapter(provider);
        let mut r = reference("refunds");
        r.parent = Some("payment_123".into());
        let prep = a.provider.read(&r).unwrap();
        let mut v = object(&a.provider, "pending");
        assert!(
            a.reduce(wrap(&a.provider, "refunds", v.clone()), &prep)
                .is_err()
        );
        v["paymentId"] = json!("different");
        v["payment_id"] = json!("different");
        v["links"] = json!({"payment":"different"});
        assert!(a.reduce(wrap(&a.provider, "refunds", v), &prep).is_err());
    }
    for (provider, pointer) in [
        (
            Provider::Mollie { test_mode: None },
            "/_links/checkout/href",
        ),
        (providers().remove(3), "/checkoutLink"),
        (providers().remove(4), "/payment_url"),
    ] {
        let value = match pointer {
            "/_links/checkout/href" => {
                json!({"_links":{"checkout":{"href":"https://checkout.example"}}})
            }
            "/checkoutLink" => json!({"checkoutLink":"https://checkout.example"}),
            _ => json!({"payment_url":"https://checkout.example"}),
        };
        assert_eq!(provider.action(&value).unwrap(), "https://checkout.example");
        assert!(provider.action(&json!({})).is_none());
    }
}
#[tokio::test]
async fn transport_failures_never_retry_or_mint_a_claim() {
    for response in [
        "".to_string(),
        "HTTP/1.1 302 Found\r\nLocation: http://outside.example/\r\nContent-Length: 0\r\n\r\n"
            .into(),
        "HTTP/1.1 500 Error\r\nContent-Length: 0\r\n\r\n".into(),
        "HTTP/1.1 200 OK\r\nContent-Length: 9000\r\n\r\n".into(),
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nxx".into(),
        "HTTP/1.1 200 OK\r\nContent-Length: 99\r\n\r\n{".into(),
        format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n",
            9000,
            "x".repeat(9000)
        ),
    ] {
        let (base, calls) = server(vec![response]).await;
        let a = local(Provider::Mollie { test_mode: None }, base);
        let result = a
            .execute(
                &instruction("mollie.payment.create", terms(&a.provider)),
                "key",
                None,
            )
            .await
            .unwrap();
        assert_eq!(result.state, State::Unknown);
        assert!(result.claim.is_none());
        assert!(result.continuation.is_none());
        assert_eq!(calls.await.unwrap().len(), 1);
    }
    let (base, calls) = server(vec!["".into()]).await;
    let a = local(Provider::Mollie { test_mode: None }, base);
    let r = encode(&reference("payments"));
    let observed = a.observe("mollie.payment.create", Some(&r)).await.unwrap();
    assert_eq!(observed.state, State::Unknown);
    assert_eq!(observed.continuation.unwrap().expose(), r.expose());
    calls.await.unwrap();
    assert!(a.observe("mollie.payment.create", None).await.is_err());
    assert!(a.observe("alien", None).await.is_err());
    assert!(
        a.execute(&instruction("alien", json!({})), "key", None)
            .await
            .is_err()
    );
    assert!(
        a.execute(
            &instruction(
                "mollie.payment.create",
                json!({"metadata":"x".repeat(8193)})
            ),
            "key",
            None
        )
        .await
        .is_err()
    );
    assert!(
        a.execute(
            &instruction("mollie.payment.create", json!([])),
            "key",
            None
        )
        .await
        .is_err()
    );
    assert!(
        a.execute(
            &instruction("mollie.payment.create", json!({"routing":[]})),
            "key",
            None
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn oauth_test_mode_stays_with_the_adapter_and_member_tokens_do_not_override_terms() {
    let provider = Provider::Mollie {
        test_mode: Some(true),
    };
    let mut pending = object(&provider, "open");
    pending["_links"] = json!({"checkout":{"href":"https://checkout.example/private"}});
    let (base, calls) = server(vec![
        http(pending.clone()),
        http(pending.clone()),
        http(pending),
    ])
    .await;
    let a = local(provider, base);
    let mut request = instruction("mollie.payment.create", terms(&a.provider));
    request.evidence = Private::new(b"\"member-token\"".to_vec());
    let result = a.execute(&request, "key", None).await.unwrap();
    assert!(result.member_action.is_some());
    a.observe(&request.operation, result.continuation.as_ref())
        .await
        .unwrap();
    a.execute(
        &instruction("mollie.payment.cancel", json!({})),
        "cancel",
        result.continuation.as_ref(),
    )
    .await
    .unwrap();
    let calls = calls.await.unwrap();
    assert!(calls[0].contains("\"testmode\":true"));
    assert!(calls[0].contains("member-token"));
    assert!(calls[1].starts_with("GET /payments/id_123?testmode=true "));
    assert!(calls[2].starts_with("DELETE /payments/id_123 "));
    assert!(calls[2].contains("\"testmode\":true"));
    let mut t = terms(&a.provider);
    t["testmode"] = json!(false);
    assert!(
        a.execute(&instruction("mollie.payment.create", t), "key", None)
            .await
            .is_err()
    );
    let a = adapter(Provider::Mollie {
        test_mode: Some(false),
    });
    assert_eq!(
        a.provider
            .prepare(
                "payment.create",
                terms(&a.provider).as_object().unwrap().clone(),
                "k",
                None
            )
            .unwrap()
            .body["testmode"],
        false
    );
    for field in ["applicationFee", "app_fee"] {
        assert!(forbidden(&json!({field: {"amount":1}})));
    }
}
#[test]
fn validated_partial_ids_survive_without_claims_and_holds_keep_expiry() {
    for (provider, status, field) in [
        (
            Provider::Mollie { test_mode: None },
            "authorized",
            "captureBefore",
        ),
        (providers().remove(2), "APPROVED", "delayed_until"),
    ] {
        let a = adapter(provider);
        let prep = a.provider.read(&reference("payments")).unwrap();
        let mut value = object(&a.provider, status);
        value[field] = json!("2030-01-01T00:00:00Z");
        let result = a
            .reduce(wrap(&a.provider, "payments", value.clone()), &prep)
            .unwrap();
        assert_eq!(result.valid_until, Some(1893456000));
        value.as_object_mut().unwrap().remove(field);
        assert_eq!(
            a.reduce(wrap(&a.provider, "payments", value.clone()), &prep)
                .unwrap()
                .valid_until,
            None
        );
        for expiry in [json!(false), json!("bad"), json!("1960-01-01T00:00:00Z")] {
            value[field] = expiry;
            let result = a
                .reduce(wrap(&a.provider, "payments", value.clone()), &prep)
                .unwrap();
            assert_eq!(result.state, State::Unknown);
            assert!(result.claim.is_none());
            assert!(result.continuation.is_some());
        }
        value.as_object_mut().unwrap().remove(field);
        let expected = Amount {
            value: "20".into(),
            currency: "CHF".into(),
        };
        let prep = Prepared {
            amount: expected,
            ..prep
        };
        let result = a
            .reduce(wrap(&a.provider, "payments", value), &prep)
            .unwrap();
        assert_eq!(result.state, State::Unknown);
        assert!(result.claim.is_none());
        assert!(result.continuation.is_some());
    }
    let gc = Provider::GoCardless { sandbox: false };
    let mut refund = object(&gc, "paid");
    refund["currency"] = json!("EUR");
    assert!(gc.state(&refund, "refunds", &amount()).is_err());
}

#[test]
fn money_parsing_never_rounds_json_or_excess_decimal_precision() {
    let provider = Provider::CoinGate { sandbox: false };
    let value: Value =
        serde_json::from_str(r#"{"price_amount":1.0000000000000000001,"price_currency":"CHF"}"#)
            .unwrap();
    let actual = provider
        .prepare(
            "order.create",
            value.as_object().unwrap().clone(),
            "key",
            None,
        )
        .unwrap()
        .amount;
    assert_eq!(actual.value, "1.0000000000000000001");
    assert!(!actual.matches(Amount {
        value: "1".into(),
        currency: "CHF".into()
    }));
    let excess: Value = serde_json::from_str(
        r#"{"price_amount":1.00000000000000000000000000001,"price_currency":"CHF"}"#,
    )
    .unwrap();
    assert!(
        provider
            .prepare(
                "order.create",
                excess.as_object().unwrap().clone(),
                "key",
                None
            )
            .is_err()
    );
    assert!(
        Amount {
            value: "1.00000000000000000000000000001".into(),
            currency: "CHF".into()
        }
        .validate()
        .is_err()
    );
}

fn fixture_mode(provider: &Provider) -> Mode {
    match provider {
        Provider::Mollie {
            test_mode: Some(true),
        }
        | Provider::GoCardless { sandbox: true }
        | Provider::Square { sandbox: true, .. }
        | Provider::CoinGate { sandbox: true } => Mode::Sandbox,
        _ => Mode::Production,
    }
}
fn configured(id: String, provider: Provider, credential: Private, limits: Limits) -> Result<Rest> {
    let mode = fixture_mode(&provider);
    Rest::new(id, provider, credential, limits, mode)
}
fn build(
    id: String,
    provider: Provider,
    credential: Private,
    limits: Limits,
    fixture: Option<Url>,
) -> Result<Rest> {
    let mode = fixture_mode(&provider);
    Rest::build(id, provider, credential, limits, fixture, mode)
}
impl Rest {
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
        let mut request = instruction(operation, serde_json::json!({}));
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
async fn configured_modes_and_action_kinds_are_checked_before_transport() {
    for (provider, mode) in [
        (Provider::GoCardless { sandbox: false }, Mode::Simulation),
        (
            Provider::Square {
                sandbox: true,
                version: "2026-09-16".into(),
            },
            Mode::Production,
        ),
    ] {
        assert!(
            Rest::new(
                "fixture".into(),
                provider,
                Private::new(b"fixture-secret".to_vec()),
                limits(),
                mode
            )
            .is_err()
        );
    }
    let adapter = adapter(Provider::GoCardless { sandbox: false });
    assert_eq!(Adapter::mode(&adapter), Mode::Production);
    let mut request = instruction("gocardless.payment.create", terms(&adapter.provider));
    request.kind = Action::Observe;
    let dispatch = Dispatch::new(
        &request,
        crate::test_support::permit(&request, Mode::Production),
        0,
        std::sync::Arc::new(|| 0),
    )
    .unwrap();
    assert!(
        Adapter::execute(&adapter, &dispatch, "key", None)
            .await
            .is_err()
    );
    request.kind = Action::Execute;
    let dispatch = Dispatch::new(
        &request,
        crate::test_support::permit(&request, Mode::Production),
        0,
        std::sync::Arc::new(|| 0),
    )
    .unwrap();
    assert!(Adapter::observe(&adapter, &dispatch, None).await.is_err());
}

#[tokio::test]
async fn preparing_terms_cannot_extend_the_transport_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let adapter = build(
        "fixture".into(),
        Provider::GoCardless { sandbox: false },
        Private::new(b"fixture-secret".to_vec()),
        limits(),
        Some(Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap()),
    )
    .unwrap();
    let request = instruction("gocardless.payment.create", terms(&adapter.provider));
    let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let clock_reads = reads.clone();
    let dispatch = Dispatch::new(
        &request,
        crate::test_support::permit(&request, Mode::Production),
        0,
        std::sync::Arc::new(move || {
            if clock_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
                0
            } else {
                100
            }
        }),
    )
    .unwrap();
    assert!(matches!(
        Adapter::execute(&adapter, &dispatch, "key", None).await,
        Err(Error::Expired)
    ));
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[path = "provider_safety.rs"]
mod safety;
