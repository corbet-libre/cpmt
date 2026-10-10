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
        crate::payments::identifier(&request.level)?;
        if request.period.start >= request.period.end
            || !outcome.matches_request(
                instruction,
                self.instruction_digest(instruction, outcome.mode()),
            )
        {
            return Err(Error::Rejected);
        }
        let evidence = outcome.evidence(
            &instruction.binding,
            request.requires,
            now,
            request.period.end,
        )?;
        let (_, end) = evidence.in_context(&instruction.binding, now)?;
        if request.period.start >= end {
            return Err(Error::Expired);
        }
        Ok(PaidAccess {
            evidence,
            level: request.level,
            period: Period {
                start: request.period.start,
                end,
            },
        })
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "tests/access.rs"]
mod tests;
