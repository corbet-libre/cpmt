//! Costed payment cascades over the shared deterministic planner.
use crate::{cost, details::Disclosure, payments::*};

/// One explicitly enabled, fully configured alternative for the requested task.
pub struct Task {
    /// Authenticated provider-native financial instruction.
    pub instruction: Instruction,
    /// External fact required for this task; no automatic downgrade is allowed.
    pub requires: State,
}

/// Why a cascade stopped. No private provider diagnostic appears here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The last provider result determines completion or the need to wait.
    Outcome(cost::Outcome),
    /// Budget, expiry or candidate exhaustion stopped the shared planner.
    Plan(cost::Error),
    /// Domain refusal or an uncertain dispatch error stopped execution.
    Domain(Error),
}

/// Results from all attempted alternatives, including billable failures.
pub struct Report {
    /// Normalized results and explicitly requested transient provider details.
    pub attempts: Vec<Execution>,
    /// Known charges plus conservatively reserved exposure.
    pub exposure: cost::Amount,
    /// Original unbudgeted/foreign-unit fee; `exposure` alone is not total cost
    /// when this is present. Host accounting must preserve prior plus reported.
    pub charge_violation: Option<cost::ChargeViolation>,
    /// Explicit stop reason; uncertainty never silently starts another payment.
    pub stop: Stop,
}

pub(crate) fn classify(state: State, required: State) -> cost::Outcome {
    match state {
        State::Unknown => cost::Outcome::Uncertain,
        State::Pending | State::RequiresAction => cost::Outcome::Pending,
        State::Failed => cost::Outcome::SafeFailure,
        _ if state == required => cost::Outcome::Success,
        _ => cost::Outcome::Refused,
    }
}

// Validate every eligible alternative before the first possible side effect.
// Provider-specific terms can differ; context and requested meaning cannot.
pub(crate) fn validate_tasks<'a>(
    plan: &cost::Plan<String, String>,
    tasks: impl IntoIterator<Item = (&'a Instruction, State)>,
) -> Result<Vec<usize>> {
    let tasks: Vec<_> = tasks.into_iter().collect();
    let mut context = None;
    let mut selected = Vec::new();
    for offer in plan.offers() {
        let mut matching = tasks
            .iter()
            .enumerate()
            .filter(|(_, (i, _))| i.adapter == offer.candidate.id);
        let (index, (instruction, requires)) = matching.next().ok_or(Error::Invalid)?;
        if matching.next().is_some()
            || context.is_some_and(|(binding, required)| {
                binding != &instruction.binding || required != *requires
            })
            || offer.quote.binding != cost::binding(instruction, plan.mode())
        {
            return Err(Error::Invalid);
        }
        context = Some((&instruction.binding, *requires));
        selected.push(index);
    }
    Ok(selected)
}

impl<J: Journal> Payments<J> {
    /// Execute exactly the caller's shared plan. This consumes no hidden fallback
    /// order. Each task must independently establish the same requested meaning.
    /// The caller's atomic state port enforces budgets across processes/restarts.
    pub async fn run(
        &self,
        plan: cost::Plan<String, String>,
        tasks: &[Task],
        now: u64,
        disclosure: Disclosure,
    ) -> Report {
        let preflight = validate_tasks(&plan, tasks.iter().map(|t| (&t.instruction, t.requires)))
            .and_then(|selected| {
                for (offer, index) in plan.offers().iter().zip(&selected) {
                    let task = &tasks[*index];
                    let quote = self.preview(
                        &task.instruction,
                        offer.quote.price.clone(),
                        offer.quote.source.clone(),
                        offer.quote.valid_until,
                    )?;
                    if quote.binding != offer.quote.binding {
                        return Err(Error::Rejected);
                    }
                }
                Ok(selected)
            });
        let selected = match preflight {
            Ok(selected) => selected,
            Err(error) => {
                return Report {
                    attempts: Vec::new(),
                    exposure: cost::Amount {
                        unit: plan.budget().unit.clone(),
                        atoms: 0,
                    },
                    stop: Stop::Domain(error),
                    charge_violation: None,
                };
            }
        };
        let mut cascade = plan.start();
        let mut attempts = Vec::new();
        let mut stop = Stop::Outcome(cost::Outcome::SafeFailure);
        for index in selected {
            let permit = match cascade.begin(self.current_time(now)) {
                Ok(permit) => permit,
                Err(error) => {
                    stop = Stop::Plan(error);
                    break;
                }
            };
            let task = &tasks[index];
            let result = match self
                .dispatch_with_output(&task.instruction, permit, now, disclosure)
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    stop = Stop::Domain(error);
                    break;
                }
            };
            let outcome = classify(result.outcome.state(), task.requires);
            let actual = result
                .cost
                .violation
                .as_ref()
                .map(cost::ReportedFee::amount)
                .or_else(|| {
                    result.cost.actual.map(|atoms| cost::Amount {
                        unit: cost::Unit {
                            code: result.cost.unit.clone(),
                            scale: result.cost.scale,
                        },
                        atoms,
                    })
                });
            attempts.push(result);
            if let Err(error) = cascade.complete(outcome, actual) {
                stop = Stop::Plan(error);
                break;
            }
            if outcome != cost::Outcome::SafeFailure {
                stop = Stop::Outcome(outcome);
                break;
            }
        }
        Report {
            attempts,
            exposure: cascade.exposure(),
            charge_violation: cascade.charge_violation().cloned(),
            stop,
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "tests/run.rs"]
mod tests;
