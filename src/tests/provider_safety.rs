use super::*;
use crate::{
    access::{Period, Request},
    cost,
    details::Disclosure,
    payments::{MemoryJournal, Payments},
    run::{Stop, Task},
};
use std::sync::Arc;

fn access(requires: State) -> Request {
    Request {
        level: "supporter".into(),
        period: Period { start: 1, end: 80 },
        requires,
    }
}

async fn uncertain_payment_stops_and_reconciles(
    factory: fn() -> Provider,
    initial: Value,
    settled_status: &str,
    resource: &str,
    settled_state: State,
) {
    let provider = factory();
    let operation = format!("{}.{}", provider.name(), provider.operations()[0]);
    let (base, calls) = server(vec![
        http(wrap(&provider, resource, initial.clone())),
        http(wrap(&provider, resource, object(&provider, settled_status))),
    ])
    .await;
    let fallback = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut alternative = local(
        factory(),
        Url::parse(&format!("http://{}/", fallback.local_addr().unwrap())).unwrap(),
    );
    alternative.id = "fallback".into();
    let mut payments = Payments::new(MemoryJournal::default(), [5; 32], Arc::new(|| 1)).unwrap();
    payments.register(Arc::new(local(factory(), base))).unwrap();
    payments.register(Arc::new(alternative)).unwrap();
    let tasks: Vec<_> = ["fixture", "fallback"]
        .into_iter()
        .enumerate()
        .map(|(index, id)| {
            let mut request = instruction(&operation, terms(&provider));
            request.id = [index as u8 + 1; 32];
            request.adapter = id.into();
            Task {
                instruction: request,
                requires: settled_state,
            }
        })
        .collect();
    let fee = cost::Amount {
        unit: cost::Unit {
            code: "fixture-fee".into(),
            scale: 0,
        },
        atoms: 1,
    };
    let plan = cost::Plan::new(
        tasks
            .iter()
            .map(|task| cost::Offer {
                candidate: cost::Candidate::new(
                    task.instruction.adapter.clone(),
                    [task.instruction.operation.clone()],
                ),
                quote: payments
                    .preview(
                        &task.instruction,
                        cost::Price::AtMost(fee.clone()),
                        "fixture tariff".into(),
                        100,
                    )
                    .unwrap(),
            })
            .collect(),
        cost::Config::new(["fixture".into(), "fallback".into()], [operation]),
        cost::CostOrder::Configured,
        cost::Amount {
            atoms: 2,
            ..fee.clone()
        },
        2,
        Mode::Production,
        1,
    )
    .unwrap();
    let report = payments.run(plan, &tasks, 1, Disclosure::Complete).await;
    assert_eq!(report.stop, Stop::Outcome(cost::Outcome::Uncertain));
    assert_eq!(report.attempts.len(), 1);
    assert_eq!(report.exposure, fee);
    assert_eq!(report.attempts[0].outcome.state(), State::Unknown);
    assert_eq!(
        serde_json::from_slice::<Value>(report.attempts[0].upstream[0].payload.expose()).unwrap(),
        wrap(&provider, resource, initial)
    );
    let request = &tasks[0].instruction;
    assert!(
        payments
            .paid_access(
                request,
                &report.attempts[0].outcome,
                access(settled_state),
                1
            )
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), fallback.accept())
            .await
            .is_err()
    );
    let read = payments
        .observation(&request.id, &request.binding, [3; 32], 100)
        .await
        .unwrap();
    let settled = payments.execute(&read, 1).await.unwrap();
    assert_eq!(settled.outcome.state(), settled_state);
    assert!(
        payments
            .paid_access(&read, &settled.outcome, access(settled_state), 1)
            .is_ok()
    );
    let calls = calls.await.unwrap();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].starts_with(&format!("POST /{resource} ")));
    assert!(calls[1].starts_with(&format!("GET /{resource}/id_123 ")));
}

#[tokio::test]
async fn coingate_late_settlement_blocks_fallback_and_reconciles_without_another_charge() {
    for status in ["expired", "invalid", "canceled"] {
        uncertain_payment_stops_and_reconciles(
            || Provider::CoinGate { sandbox: false },
            object(&Provider::CoinGate { sandbox: false }, status),
            "paid",
            "orders",
            State::Settled,
        )
        .await;
    }
}

#[tokio::test]
async fn gocardless_automatic_retry_blocks_fallback_until_read_only_reconciliation() {
    for retry in [Value::Null, json!(true), json!("false")] {
        let mut initial = object(&Provider::GoCardless { sandbox: false }, "failed");
        initial["retry_if_possible"] = retry;
        uncertain_payment_stops_and_reconciles(
            || Provider::GoCardless { sandbox: false },
            initial,
            "confirmed",
            "payments",
            State::Collected,
        )
        .await;
    }
}

#[tokio::test]
async fn square_hold_evidence_requires_actual_bank_authorization_and_keeps_reconciliation() {
    for approved in [
        Value::Null,
        json!("10 CHF"),
        json!({"amount":0,"currency":"CHF"}),
        json!({"amount":5,"currency":"CHF"}),
        json!({"amount":10,"currency":"EUR"}),
    ] {
        let provider = providers().remove(2);
        let mut initial = object(&provider, "APPROVED");
        initial["approved_money"] = approved;
        let mut authorized = object(&provider, "APPROVED");
        authorized["approved_money"] = json!({"amount":13,"currency":"CHF"});
        authorized["tip_money"] = json!({"amount":3,"currency":"CHF"});
        authorized["delayed_until"] = json!("1970-01-01T00:00:20Z");
        let initial = wrap(&provider, "payments", initial);
        let (base, calls) = server(vec![
            http(initial.clone()),
            http(wrap(&provider, "payments", authorized)),
        ])
        .await;
        let mut payments =
            Payments::new(MemoryJournal::default(), [5; 32], Arc::new(|| 1)).unwrap();
        let request = instruction("square.payment.create", terms(&provider));
        payments.register(Arc::new(local(provider, base))).unwrap();
        let result = payments
            .dispatch_with_output(
                &request,
                crate::test_support::permit(&request, Mode::Production),
                1,
                Disclosure::Complete,
            )
            .await
            .unwrap();
        assert_eq!(result.outcome.state(), State::Unknown);
        assert!(!result.outcome.satisfies_terms());
        assert_eq!(
            serde_json::from_slice::<Value>(result.upstream[0].payload.expose()).unwrap(),
            initial
        );
        assert!(
            payments
                .paid_access(&request, &result.outcome, access(State::Authorized), 1)
                .is_err()
        );
        let read = payments
            .observation(&request.id, &request.binding, [2; 32], 100)
            .await
            .unwrap();
        let authorized = payments.execute(&read, 1).await.unwrap();
        let proof = payments
            .paid_access(&read, &authorized.outcome, access(State::Authorized), 1)
            .unwrap();
        assert_eq!(
            proof.in_context(&read.binding, 1).unwrap(),
            ("supporter", Period { start: 1, end: 20 })
        );
        assert!(proof.in_context(&read.binding, 20).is_err());
        let calls = calls.await.unwrap();
        assert!(calls[0].starts_with("POST /payments "));
        assert!(calls[1].starts_with("GET /payments/id_123 "));
    }
}
