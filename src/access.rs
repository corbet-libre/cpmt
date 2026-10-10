//! Transform explicitly configured financial conditions into paid access.
use crate::payments::{
    Binding, Error, Evidence, Instruction, Journal, Outcome, Payments, Result, State,
};

/// Requested access interval. The end is exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Period {
    /// Inclusive start, in the caller's clock convention.
    pub start: u64,
    /// Exclusive end, capped by any authenticated provider expiry.
    pub end: u64,
}

/// Caller-owned interpretation of the exact financial terms in an instruction.
/// The library never chooses the price, required financial fact, level or period.
#[derive(Clone, Debug)]
pub struct Request {
    /// Application-defined level, without an invented ranking.
    pub level: String,
    /// Requested validity interval.
    pub period: Period,
    /// Fact the caller requires, for example collection or an authorized hold.
    pub requires: State,
}

/// Paid-access fact. Provider amounts, currencies and object IDs stay private.
/// There is no deserialization or public constructor for this proof.
pub struct PaidAccess {
    evidence: Evidence,
    level: String,
    period: Period,
}

impl PaidAccess {
    /// Recheck the authenticated context and interval before consuming the fact.
    pub fn in_context(&self, binding: &Binding, now: u64) -> Result<(&str, Period)> {
        self.evidence.in_context(binding, now)?;
        if now < self.period.start {
            return Err(Error::Expired);
        }
        Ok((&self.level, self.period))
    }
    /// Minimal in-process gate input; the gate owner enforces its consumption rule.
    pub fn evidence(&self) -> &Evidence {
        &self.evidence
    }
}

/// Explicit dummy access transformation. This is data, never production evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimulatedAccess {
    /// Caller-selected level after the actual domain validation.
    pub level: String,
    /// Requested interval capped by the supplied dummy provider expiry.
    pub period: Period,
}
fn project(request: Request, provider_until: Option<u64>, now: u64) -> Result<SimulatedAccess> {
    crate::payments::identifier(&request.level)?;
    if request.period.start >= request.period.end {
        return Err(Error::Rejected);
    }
    let end = provider_until.map_or(request.period.end, |limit| request.period.end.min(limit));
    if request.period.start >= end || now >= end {
        return Err(Error::Expired);
    }
    Ok(SimulatedAccess {
        level: request.level,
        period: Period {
            start: request.period.start,
            end,
        },
    })
}
/// Apply the real paid-access transformation to explicit dummy facts, with no I/O.
/// This result has no evidence conversion and cannot be used as a production gate.
pub fn simulate_access(
    request: Request,
    state: State,
    satisfies_terms: bool,
    provider_until: Option<u64>,
    now: u64,
) -> Result<SimulatedAccess> {
    if !satisfies_terms
        || request.requires != state
        || matches!(
            state,
            State::Pending | State::RequiresAction | State::Unknown | State::Failed
        )
    {
        return Err(Error::Rejected);
    }
    project(request, provider_until, now)
}
impl<J: Journal> Payments<J> {
    /// Project only a result that established this exact instruction's financial
    /// terms. Complete upstream JSON cannot be substituted for an `Outcome`.
    pub fn paid_access(
        &self,
        instruction: &Instruction,
        outcome: &Outcome,
        request: Request,
        now: u64,
    ) -> Result<PaidAccess> {
        if !outcome.matches_request(
            instruction,
            self.instruction_digest(instruction, outcome.mode()),
        ) {
            return Err(Error::Rejected);
        }
        let evidence = outcome.evidence(
            &instruction.binding,
            request.requires,
            now,
            request.period.end,
        )?;
        let (_, end) = evidence.in_context(&instruction.binding, now)?;
        let projected = project(request, Some(end), now)?;
        Ok(PaidAccess {
            evidence,
            level: projected.level,
            period: projected.period,
        })
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "tests/access.rs"]
mod tests;
