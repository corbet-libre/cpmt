use super::*;
use crate::{simulation, test_support};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct Provider {
    id: String,
    state: State,
    fee: Option<cost::Amount>,
    calls: Arc<AtomicUsize>,
    mode: cost::Mode,
    fails: bool,
}
impl Adapter for Provider {
    fn max_input_bytes(&self) -> usize {
        16384
    }
    fn mode(&self) -> cost::Mode {
        self.mode
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            id: self.id.clone(),
            operations: vec!["collect".into()],
        }
    }
    async fn execute(
        &self,
        dispatch: &cost::Dispatch<'_>,
        _: &str,
        _: Option<&Private>,
    ) -> Result<Observation> {
        dispatch.check(&self.id, self.mode)?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fails {
            return Err(Error::Rejected);
        }
        Ok(Observation {
            actual_cost: self.fee.clone(),
            ..Observation::fact(self.state)
        })
    }
    async fn observe(&self, _: &cost::Dispatch<'_>, _: Option<&Private>) -> Result<Observation> {
        unreachable!("execute-only fixture")
    }
}
fn fee(atoms: u64) -> cost::Amount {
    cost::Amount {
        unit: cost::Unit {
            code: "fixture-fee".into(),
            scale: 0,
        },
        atoms,
    }
}
fn tasks() -> Vec<Task> {
    ["first", "second"]
        .into_iter()
        .enumerate()
        .map(|(n, id)| {
            let mut instruction = crate::payments::tests::request();
            instruction.adapter = id.into();
            instruction.id = [n as u8 + 1; 32];
            Task {
                instruction,
                requires: State::Collected,
            }
        })
        .collect()
}
fn plan(tasks: &[Task], mode: cost::Mode, budget: u64, until: u64) -> cost::Plan<String, String> {
    cost::Plan::new(
        tasks
            .iter()
            .map(|t| cost::Offer {
                candidate: cost::Candidate::new(t.instruction.adapter.clone(), ["collect".into()]),
                quote: cost::Quote {
                    price: cost::Price::AtMost(fee(5)),
                    source: "inert fixture tariff".into(),
                    valid_until: until,
                    binding: cost::binding(&t.instruction, mode),
                },
            })
            .collect(),
        cost::Config::new(
            tasks.iter().map(|t| t.instruction.adapter.clone()),
            ["collect".into()],
        ),
        cost::CostOrder::Configured,
        fee(budget),
        2,
        mode,
        0,
    )
    .unwrap()
}
fn setup(
    states: [State; 2],
    actual: Option<u64>,
    mode: cost::Mode,
    fails: bool,
) -> (Payments<MemoryJournal>, Arc<AtomicUsize>) {
    let mut payments = Payments::new(MemoryJournal::default(), [5; 32]).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    for (id, state) in ["first", "second"].into_iter().zip(states) {
        payments
            .register(Arc::new(Provider {
                id: id.into(),
                state,
                fee: actual.map(fee),
                calls: calls.clone(),
                mode,
                fails,
            }))
            .unwrap();
    }
    (payments, calls)
}
#[tokio::test]
async fn billed_failure_uses_remaining_cascade_budget_and_stops_on_success() {
    let tasks = tasks();
    let (payments, calls) = setup(
        [State::Failed, State::Collected],
        Some(2),
        cost::Mode::Production,
        false,
    );
    let report = payments
        .run(
            plan(&tasks, cost::Mode::Production, 7, 100),
            &tasks,
            1,
            Disclosure::Complete,
        )
        .await;
    assert_eq!(report.stop, Stop::Outcome(cost::Outcome::Success));
    assert_eq!(report.exposure, fee(4));
    assert_eq!(report.attempts.len(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let (payments, calls) = setup(
        [State::Failed, State::Collected],
        None,
        cost::Mode::Production,
        false,
    );
    let report = payments
        .run(
            plan(&tasks, cost::Mode::Production, 7, 100),
            &tasks,
            1,
            Disclosure::Minimal,
        )
        .await;
    assert_eq!(report.stop, Stop::Plan(cost::Error::Budget));
    assert_eq!(report.exposure, fee(5));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn ambiguous_pending_refused_expired_and_domain_failures_never_fallback() {
    for (state, expected) in [
        (State::Unknown, cost::Outcome::Uncertain),
        (State::Pending, cost::Outcome::Pending),
        (State::RequiresAction, cost::Outcome::Pending),
        (State::Authorized, cost::Outcome::Refused),
    ] {
        let tasks = tasks();
        let (payments, calls) = setup(
            [state, State::Collected],
            None,
            cost::Mode::Production,
            false,
        );
        let report = payments
            .run(
                plan(&tasks, cost::Mode::Production, 10, 100),
                &tasks,
                1,
                Disclosure::Minimal,
            )
            .await;
        assert_eq!(report.stop, Stop::Outcome(expected));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.exposure, fee(5));
    }
    let tasks = tasks();
    let (payments, calls) = setup([State::Collected; 2], None, cost::Mode::Production, true);
    let report = payments
        .run(
            plan(&tasks, cost::Mode::Production, 10, 1),
            &tasks,
            1,
            Disclosure::Minimal,
        )
        .await;
    assert_eq!(report.stop, Stop::Plan(cost::Error::Expired));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let report = payments
        .run(
            plan(&tasks, cost::Mode::Production, 10, 100),
            &tasks,
            1,
            Disclosure::Minimal,
        )
        .await;
    assert_eq!(report.stop, Stop::Domain(Error::Rejected));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(report.exposure, fee(5));
}
#[tokio::test]
async fn every_alternative_is_checked_before_any_charge() {
    let (payments, calls) = setup([State::Collected; 2], None, cost::Mode::Production, false);
    for case in 0..5 {
        let mut tasks = tasks();
        let plan = plan(&tasks, cost::Mode::Production, 10, 100);
        match case {
            0 => {
                tasks.pop();
            }
            1 => tasks[1].instruction.binding.subject.push('x'),
            2 => tasks[1].requires = State::Refunded,
            3 => tasks[1].instruction.terms = Private::new(b"changed".to_vec()),
            _ => {
                let mut extra = crate::payments::tests::request();
                extra.adapter = "first".into();
                tasks.push(Task {
                    instruction: extra,
                    requires: State::Collected,
                });
            }
        }
        assert_eq!(
            payments
                .run(plan, &tasks, 1, Disclosure::Minimal)
                .await
                .stop,
            Stop::Domain(Error::Invalid)
        );
    }
    let tasks = tasks();
    assert_eq!(
        payments
            .run(
                plan(&tasks, cost::Mode::Sandbox, 10, 100),
                &tasks,
                1,
                Disclosure::Minimal
            )
            .await
            .stop,
        Stop::Domain(Error::Rejected)
    );
    let empty = Payments::new(MemoryJournal::default(), [2; 32]).unwrap();
    assert_eq!(
        empty
            .run(
                plan(&tasks, cost::Mode::Production, 10, 100),
                &tasks,
                1,
                Disclosure::Minimal
            )
            .await
            .stop,
        Stop::Domain(Error::Unsupported)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // A production permit still requires its exact instruction at the transport edge.
    let permit = test_support::permit(&tasks[0].instruction, cost::Mode::Production);
    assert!(cost::Dispatch::new(&tasks[1].instruction, permit, 1).is_err());
}
#[test]
fn simulation_uses_real_budget_fallback_and_binding_without_production_proofs() {
    let tasks = tasks();
    let make = || {
        tasks
            .iter()
            .map(|t| {
                let mut instruction = crate::payments::tests::request();
                instruction.adapter = t.instruction.adapter.clone();
                instruction.id = t.instruction.id;
                simulation::Fixture {
                    instruction,
                    requires: State::Collected,
                    state: if t.instruction.adapter == "first" {
                        State::Failed
                    } else {
                        State::Collected
                    },
                    actual_cost: Some(fee(2)),
                }
            })
            .collect::<Vec<_>>()
    };
    let result = simulation::simulate(plan(&tasks, cost::Mode::Simulation, 7, 100), &make(), 1);
    assert_eq!(result.states, [State::Failed, State::Collected]);
    assert_eq!(result.exposure, fee(4));
    assert_eq!(result.stop, Stop::Outcome(cost::Outcome::Success));
    for mode in [cost::Mode::Production, cost::Mode::Sandbox] {
        let result = simulation::simulate(plan(&tasks, mode, 7, 100), &make(), 1);
        assert!(result.states.is_empty());
        assert_eq!(result.stop, Stop::Plan(cost::Error::Binding));
    }
    assert_eq!(
        simulation::simulate(plan(&tasks, cost::Mode::Simulation, 7, 1), &make(), 1).stop,
        Stop::Plan(cost::Error::Expired)
    );
    assert_eq!(
        simulation::simulate(plan(&tasks, cost::Mode::Simulation, 7, 100), &[], 1).stop,
        Stop::Domain(Error::Invalid)
    );
    let mut fixtures = make();
    fixtures[0].actual_cost = Some(fee(6));
    assert_eq!(
        simulation::simulate(plan(&tasks, cost::Mode::Simulation, 7, 100), &fixtures, 1).stop,
        Stop::Plan(cost::Error::Charge)
    );
}

#[tokio::test]
async fn replayed_actual_fee_cannot_fit_a_smaller_replanned_quote() {
    let tasks = tasks();
    let (payments, _) = setup(
        [State::Collected; 2],
        Some(2),
        cost::Mode::Production,
        false,
    );
    assert_eq!(
        payments
            .run(
                plan(&tasks, cost::Mode::Production, 10, 100),
                &tasks,
                1,
                Disclosure::Minimal
            )
            .await
            .stop,
        Stop::Outcome(cost::Outcome::Success)
    );
    let original = plan(&tasks, cost::Mode::Production, 10, 100);
    let mut offers = original.offers().to_vec();
    offers[0].quote.price = cost::Price::Exact(fee(1));
    let cheaper = cost::Plan::new(
        offers,
        cost::Config::new(["first".into(), "second".into()], ["collect".into()]),
        cost::CostOrder::Configured,
        fee(10),
        2,
        cost::Mode::Production,
        0,
    )
    .unwrap();
    assert_eq!(
        payments
            .run(cheaper, &tasks, 1, Disclosure::Minimal)
            .await
            .stop,
        Stop::Plan(cost::Error::Charge)
    );
}
