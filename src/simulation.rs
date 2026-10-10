//! Deterministic local payment scenarios. This module has no provider or I/O port.
use crate::{
    cost,
    payments::{Instruction, State},
    run::{Stop, classify, validate_tasks},
};

/// Explicit dummy provider outcome used only by local simulation.
pub struct Fixture {
    /// Dummy instruction used by the real quote-binding calculation.
    pub instruction: Instruction,
    /// Financial fact the caller requests.
    pub requires: State,
    /// Dummy response state, never accepted as real evidence.
    pub state: State,
    /// Explicit simulated service fee. Absence preserves maximum exposure.
    pub actual_cost: Option<cost::Amount>,
}

/// Simulated execution data. It contains no production Outcome or Evidence.
#[derive(Debug)]
pub struct Simulation {
    /// Dummy states in actual shared-plan order.
    pub states: Vec<State>,
    /// Simulated costs/reservations; no money was spent.
    pub exposure: cost::Amount,
    /// Explicit stop reason from the same planner/classification as real tasks.
    pub stop: Stop,
}

/// Exercise the real plan and request binding using supplied dummy responses.
/// No credential, transport or adapter is accepted. Even a zero-cost production
/// or sandbox plan is refused rather than interpreted as a simulation.
pub fn simulate(plan: cost::Plan<String, String>, fixtures: &[Fixture], now: u64) -> Simulation {
    if plan.mode() != cost::Mode::Simulation {
        return Simulation {
            states: Vec::new(),
            exposure: cost::Amount {
                unit: plan.budget().unit.clone(),
                atoms: 0,
            },
            stop: Stop::Plan(cost::Error::Binding),
        };
    }
    let selected =
        match validate_tasks(&plan, fixtures.iter().map(|f| (&f.instruction, f.requires))) {
            Ok(selected) => selected,
            Err(error) => {
                return Simulation {
                    states: Vec::new(),
                    exposure: cost::Amount {
                        unit: plan.budget().unit.clone(),
                        atoms: 0,
                    },
                    stop: Stop::Domain(error),
                };
            }
        };
    let mut cascade = plan.start();
    let mut states = Vec::new();
    let mut stop = Stop::Outcome(cost::Outcome::SafeFailure);
    for index in selected {
        if let Err(error) = cascade.begin(now) {
            stop = Stop::Plan(error);
            break;
        }

        let fixture = &fixtures[index];
        states.push(fixture.state);
        let outcome = classify(fixture.state, fixture.requires);
        if let Err(error) = cascade.complete(outcome, fixture.actual_cost.clone()) {
            stop = Stop::Plan(error);
            break;
        }
        if outcome != cost::Outcome::SafeFailure {
            stop = Stop::Outcome(outcome);
            break;
        }
    }
    Simulation {
        states,
        exposure: cascade.exposure(),
        stop,
    }
}
