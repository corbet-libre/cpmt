//! Pure cost previews and request binding. No pricing endpoint is contacted.
use crate::payments::{Error as PaymentError, Instruction, Result};
pub use cfbk::cost::*;
pub use cfbk::{Candidate, Config};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Trusted host clock in the same time convention as request/quote deadlines.
/// Implementations must advance during awaits; a captured admission timestamp
/// does not satisfy production freshness. Tests can inject deterministic clocks.
pub trait Clock: Send + Sync {
    /// Read current trusted time immediately before a side effect.
    fn now(&self) -> u64;
}
impl<F: Fn() -> u64 + Send + Sync> Clock for F {
    fn now(&self) -> u64 {
        self()
    }
}

/// Authenticated unexpected service charge retained for host reconciliation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReportedFee {
    /// Original unit, without conversion into the reserved denomination.
    pub unit: String,
    /// Original unit scale.
    pub scale: u8,
    /// Provider-reported integer quantity, including an excess charge.
    pub atoms: u64,
}
impl ReportedFee {
    /// Return the shared planner amount without losing denomination.
    pub fn amount(&self) -> Amount {
        Amount {
            unit: Unit {
                code: self.unit.clone(),
                scale: self.scale,
            },
            atoms: self.atoms,
        }
    }
}

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
    /// Preserved charge exceeding the quote or changing unit. Its presence
    /// blocks proof/fallback and requires host accounting beyond `maximum`.
    pub violation: Option<ReportedFee>,
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
            violation: None,
            source: permit.quote().source.clone(),
        })
    }
    pub(crate) fn report(&mut self, actual: Option<&Amount>) -> Result<()> {
        if let Some(actual) = actual {
            if actual.unit.code != self.unit
                || actual.unit.scale != self.scale
                || actual.atoms > self.maximum
            {
                self.violation = Some(ReportedFee {
                    unit: actual.unit.code.clone(),
                    scale: actual.unit.scale,
                    atoms: actual.atoms,
                });
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
    clock: Arc<dyn Clock>,
}
impl<'a> Dispatch<'a> {
    /// Bind an already reserved host operation to its explicitly authorized cost.
    pub fn new(
        request: &'a Instruction,
        permit: DispatchPermit<String>,
        now: u64,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        validate(
            request,
            &request.adapter,
            permit.mode(),
            &permit,
            now.max(clock.now()),
        )?;
        Ok(Self {
            request,
            permit,
            now,
            clock,
        })
    }
    /// Adapters must call this with their actual account instance and mode
    /// before every potentially billable operation, including observations.
    pub fn check(&self, adapter: &str, mode: Mode) -> Result<()> {
        validate(
            self.request,
            adapter,
            mode,
            &self.permit,
            self.now.max(self.clock.now()),
        )
    }
}
impl std::ops::Deref for Dispatch<'_> {
    type Target = Instruction;
    fn deref(&self) -> &Instruction {
        self.request
    }
}
