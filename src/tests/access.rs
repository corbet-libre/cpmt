use super::*;
use crate::payments::{
    MemoryJournal,
    tests::{request, setup},
};

fn access() -> Request {
    Request {
        level: "supporter".into(),
        period: Period { start: 1, end: 80 },
        requires: State::Collected,
    }
}

#[tokio::test]
async fn paid_access_binds_exact_financial_instruction_and_caps_requested_period() {
    let (payments, _) = setup(MemoryJournal::default(), false);
    let mut instruction = request();
    let result = payments.execute(&instruction, 1).await.unwrap();
    let fact = payments
        .paid_access(&instruction, &result.outcome, access(), 1)
        .unwrap();
    assert_eq!(
        fact.in_context(&instruction.binding, 1).unwrap(),
        ("supporter", Period { start: 1, end: 50 })
    );
    assert!(fact.in_context(&instruction.binding, 50).is_err());
    assert!(fact.in_context(&instruction.binding, 0).is_err());
    assert_eq!(
        fact.evidence().claim(),
        result
            .outcome
            .evidence(&instruction.binding, State::Collected, 1, 50)
            .unwrap()
            .claim()
    );
    let mut wrong = instruction.binding.clone();
    wrong.subject.push('x');
    assert!(fact.in_context(&wrong, 2).is_err());
    instruction.terms = crate::payments::Private::new(b"other-price".to_vec());
    assert!(
        payments
            .paid_access(&instruction, &result.outcome, access(), 1)
            .is_err()
    );
}

#[tokio::test]
async fn invalid_levels_periods_and_unestablished_financial_facts_do_not_grant_access() {
    let (payments, _) = setup(MemoryJournal::default(), false);
    let instruction = request();
    let result = payments.execute(&instruction, 1).await.unwrap();
    for invalid in [
        Request {
            level: String::new(),
            ..access()
        },
        Request {
            period: Period { start: 10, end: 10 },
            ..access()
        },
        Request {
            period: Period { start: 60, end: 80 },
            ..access()
        },
        Request {
            requires: State::Authorized,
            ..access()
        },
    ] {
        assert!(
            payments
                .paid_access(&instruction, &result.outcome, invalid, 1)
                .is_err()
        );
    }
    let future = payments
        .paid_access(
            &instruction,
            &result.outcome,
            Request {
                period: Period { start: 20, end: 40 },
                ..access()
            },
            1,
        )
        .unwrap();
    assert!(future.in_context(&instruction.binding, 19).is_err());
    let read = payments
        .observation(&instruction.id, &instruction.binding, [8; 32], 100)
        .await
        .unwrap();
    let observed = payments.execute(&read, 1).await.unwrap();
    assert!(
        payments
            .paid_access(
                &read,
                &observed.outcome,
                Request {
                    requires: State::Refunded,
                    ..access()
                },
                1
            )
            .is_err()
    );
}

#[test]
fn simulation_uses_the_real_level_interval_transformation_but_produces_no_evidence() {
    let simulated = simulate_access(access(), State::Collected, true, Some(50), 1).unwrap();
    assert_eq!(
        simulated,
        SimulatedAccess {
            level: "supporter".into(),
            period: Period { start: 1, end: 50 }
        }
    );
    assert_eq!(
        simulate_access(access(), State::Collected, true, None, 1)
            .unwrap()
            .period
            .end,
        80
    );
    for (state, satisfies) in [
        (State::Pending, true),
        (State::RequiresAction, true),
        (State::Unknown, true),
        (State::Failed, true),
        (State::Collected, false),
    ] {
        assert!(
            simulate_access(
                Request {
                    requires: state,
                    ..access()
                },
                state,
                satisfies,
                None,
                1
            )
            .is_err()
        );
    }
    assert!(simulate_access(access(), State::Authorized, true, None, 1).is_err());
    assert!(simulate_access(access(), State::Collected, true, None, 80).is_err());
}
