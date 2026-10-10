//! Pure cost previews and request binding. No pricing endpoint is contacted.
use crate::payments::{Error as PaymentError, Instruction, Result};
pub use cfbk::cost::*;
pub use cfbk::{Candidate, Config};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Minimal caller-owned exposure record, separate from payment principal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reservation {
    /// Explicit fee denomination; never implicitly converted.
    pub unit: String,
    /// Decimal places per integer atom.
    pub scale: u8,
    /// Maximum fee reserved before dispatch, including possible failure costs.
    pub maximum: u64,
    /// Actual authenticated fee, if reported in this same denomination.
    pub actual: Option<u64>,
    /// Provider quote or explicitly configured tariff revision.
    pub source: String,
}

impl Reservation {
    pub(crate) fn from_permit(permit: &DispatchPermit<String>) -> Result<Self> {
        let maximum = permit.quote().price.maximum().ok_or(PaymentError::Cost)?;
        Ok(Self {
            unit: maximum.unit.code.clone(),
            scale: maximum.unit.scale,
            maximum: maximum.atoms,
            actual: None,
            source: permit.quote().source.clone(),
        })
    }
    pub(crate) fn report(&mut self, actual: Option<&Amount>) -> Result<()> {
        if let Some(actual) = actual {
            if actual.unit.code != self.unit
                || actual.unit.scale != self.scale
                || actual.atoms > self.maximum
            {
                return Err(PaymentError::Cost);
            }
            self.actual = Some(actual.atoms);
        }
        Ok(())
    }
}

/// Independently derive the binding for one configured account, mode and task.
/// The random operation ID prevents public digests exposing low-entropy inputs.
pub fn binding(request: &Instruction, mode: Mode) -> String {
    let mode = match mode {
        Mode::Simulation => "simulation",
        Mode::Sandbox => "sandbox",
        Mode::Production => "production",
    };
    let bytes = zeroize::Zeroizing::new(
        serde_json::to_vec(&(
            "cpmt.cost.v1",
            mode,
            request.kind,
            request.id,
            request.parent,
            &request.adapter,
            &request.operation,
            &request.binding,
            request.expires_at,
            request.terms.expose(),
            request.evidence.expose(),
        ))
        .expect("fixed request fields serialize"),
    );
    crate::hex(&Sha256::digest(&bytes))
}

/// Validate a move-only cost authorization at the actual adapter dispatch edge.
pub fn validate(
    request: &Instruction,
    adapter: &str,
    mode: Mode,
    permit: &DispatchPermit<String>,
    now: u64,
) -> Result<()> {
    if mode == Mode::Simulation
        || request.id == [0; 32]
        || &request.adapter != permit.candidate()
        || request.adapter != adapter
    {
        return Err(PaymentError::Cost);
    }
    if now >= request.expires_at {
        return Err(PaymentError::Expired);
    }
    permit
        .validate(&binding(request, mode), mode, now)
        .map_err(|_| PaymentError::Cost)
}

/// Move-only authorized request for a low-level adapter. A host using this
/// directly must atomically reserve its operation and budget before construction;
/// [`crate::payments::Payments`] performs that protocol through its state port.
pub struct Dispatch<'a> {
    request: &'a Instruction,
    permit: DispatchPermit<String>,
    now: u64,
}
impl<'a> Dispatch<'a> {
    /// Bind an already reserved host operation to its explicitly authorized cost.
    pub fn new(request: &'a Instruction, permit: DispatchPermit<String>, now: u64) -> Result<Self> {
        validate(request, &request.adapter, permit.mode(), &permit, now)?;
        Ok(Self {
            request,
            permit,
            now,
        })
    }
    /// Adapters must call this with their actual account instance and mode
    /// before every potentially billable operation, including observations.
    pub fn check(&self, adapter: &str, mode: Mode) -> Result<()> {
        validate(self.request, adapter, mode, &self.permit, self.now)
    }
}
impl std::ops::Deref for Dispatch<'_> {
    type Target = Instruction;
    fn deref(&self) -> &Instruction {
        self.request
    }
}

/// Terminal interpretation for the shared fallback planner. A provider refusal
/// is not automatically safe to try at another payment provider.
pub fn fallback(state: crate::payments::State) -> Outcome {
    use crate::payments::State;
    match state {
        State::Unknown => Outcome::Uncertain,
        State::Pending | State::RequiresAction => Outcome::Pending,
        State::Failed => Outcome::Refused,
        _ => Outcome::Success,
    }
}
