//! Shared execution contract. Adapters own provider behavior; callers own policy.
use crate::cost::{self, Dispatch, DispatchPermit, Mode, Price, Quote, Reservation};
use crate::details::{Disclosure, ProviderReply};
use serde::{Deserialize, Serialize};
use std::{fmt, future::Future, pin::Pin};
use zeroize::Zeroizing;

/// Uniform errors never embed provider responses, addresses or identifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Missing, stale, changed or insufficient cost authorization.
    #[error("payment cost authorization rejected")]
    Cost,
    /// Invalid trusted configuration or malformed input.
    #[error("invalid payment input")]
    Invalid,
    /// This adapter cannot perform the requested operation.
    #[error("unsupported payment operation")]
    Unsupported,
    /// An existing operation identifier was reused for different instructions.
    #[error("payment operation conflict")]
    Conflict,
    /// No execution is allowed after the caller's deadline.
    #[error("payment request expired")]
    Expired,
    /// Required storage was unavailable; do not retry with a new identifier.
    #[error("payment storage unavailable")]
    Storage,
    /// The durable journal was initialized with a different binding key.
    #[error("payment journal key mismatch")]
    KeyMismatch,
    /// Evidence does not establish the requested fact.
    #[error("payment evidence rejected")]
    Rejected,
}

/// Payment result.
pub type Result<T> = std::result::Result<T, Error>;

/// Sensitive bytes, deliberately without serialization or revealing Debug.
pub struct Private(Zeroizing<Vec<u8>>);
impl Private {
    /// Take ownership of transient provider input.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }
    /// Borrow input only inside an adapter or an authenticated member handover.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}
impl fmt::Debug for Private {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Private([redacted])")
    }
}

/// Authenticated scope supplied by the composition root, never by raw evidence.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    /// Service/community scope; an opaque application value.
    pub scope: String,
    /// Authenticated member's opaque identifier in that scope.
    pub subject: String,
    /// Exact caller operation being funded or checked.
    pub action: String,
}
impl fmt::Debug for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Binding([redacted])")
    }
}

/// Trusted instructions. New operations use namespaced identifiers, not a core enum.
pub struct Instruction {
    /// Whether this task changes provider state or reads an existing operation.
    pub kind: Action,
    /// Random, unguessable operation identifier assigned once by the host.
    pub id: [u8; 32],
    /// Earlier operation in the same authenticated context, for capture/refund/etc.
    pub parent: Option<[u8; 32]>,
    /// The selected adapter, from trusted configuration.
    pub adapter: String,
    /// Adapter-advertised operation (for example `payment.collect`).
    pub operation: String,
    /// Authenticated member, service and action.
    pub binding: Binding,
    /// Exclusive deadline for starting this operation.
    pub expires_at: u64,
    /// Caller-selected commercial terms, including provider-specific parameters.
    pub terms: Private,
    /// Transient member/provider evidence; never persisted by the executor.
    pub evidence: Private,
}

/// Provider interaction requested by the caller. Reads still need cost approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    /// Explicitly execute the advertised operation once.
    Execute,
    /// Read the authenticated parent object; terms and evidence must be empty.
    Observe,
}
impl fmt::Debug for Instruction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Instruction([redacted])")
    }
}

/// Facts about an external payment. These do not grant application access.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Awaiting an external result.
    Pending,
    /// The member must complete a provider interaction.
    RequiresAction,
    /// A payment authorization was verified; this is neither a hold nor collection.
    Verified,
    /// Funds were authorized/held, but have not been collected.
    Authorized,
    /// The provider confirmed collection.
    Collected,
    /// A protocol confirmed its settlement step; its scheme defines the effect.
    Settled,
    /// A refund completed.
    Refunded,
    /// An external hold was released.
    Released,
    /// An external recurring arrangement is active; this does not prove payment.
    Active,
    /// An arrangement was cancelled.
    Cancelled,
    /// Provider confirmed refusal/failure.
    Failed,
    /// Outcome is uncertain. Reconcile; never treat this as a safe new payment.
    Unknown,
}

/// Adapter capabilities. Currency/network details belong to the adapter's terms.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    /// Unique configuration identity of this adapter instance.
    pub id: String,
    /// Supported operation identifiers. No implicit provider fallback.
    pub operations: Vec<String>,
}

/// Adapter-only result before reduction to an application-safe outcome.
pub struct Observation {
    /// The provider established the bound financial terms in full. Partial or
    /// unproven amounts must leave this false, even when an object exists.
    pub satisfies_terms: bool,
    /// Actual service fee when authenticated provider evidence establishes it.
    /// Absence is unknown, never zero. Payment principal is not a service fee.
    pub actual_cost: Option<cost::Amount>,
    /// Verified external fact.
    pub state: State,
    /// Provider's exclusive validity bound, if the fact is time limited.
    pub valid_until: Option<u64>,
    /// Minimum provider reference needed to reconcile or continue the operation.
    /// Stored within the payment boundary; never included in gate evidence.
    pub continuation: Option<Private>,
    /// Stable provider receipt identity, namespaced by protocol/platform.
    /// The executor retains only a keyed digest for the gate host to spend atomically.
    pub claim: Option<Private>,
    /// Sensitive instructions for this authenticated member only (such as 3DS).
    /// Never persisted or included in gate evidence.
    pub member_action: Option<Private>,
    /// Complete available upstream responses, never journaled or used as proof.
    pub upstream: Vec<ProviderReply>,
}
impl Observation {
    /// A minimal result without provider data.
    pub fn fact(state: State) -> Self {
        Self {
            satisfies_terms: false,
            actual_cost: None,
            state,
            valid_until: None,
            continuation: None,
            claim: None,
            member_action: None,
            upstream: Vec::new(),
        }
    }
}

/// Adapter implementation is trusted code. Never construct one from member input.
pub trait Adapter: Send + Sync {
    /// Explicit configured execution environment; never inferred as free.
    fn mode(&self) -> Mode;
    /// Explicit bound on combined terms/evidence bytes, checked before hashing.
    fn max_input_bytes(&self) -> usize;
    /// Enumerate exactly the implemented operations of this configured adapter.
    fn capabilities(&self) -> Capabilities;
    /// Execute once with the stable upstream idempotency key. Ambiguous results
    /// must be Unknown. Parent references stay inside the payment boundary.
    fn execute(
        &self,
        request: &Dispatch<'_>,
        key: &str,
        previous: Option<&Private>,
    ) -> impl Future<Output = Result<Observation>> + Send;
    /// Reconcile without initiating another charge.
    fn observe(
        &self,
        request: &Dispatch<'_>,
        continuation: Option<&Private>,
    ) -> impl Future<Output = Result<Observation>> + Send;
}

type Pending<'a> = Pin<Box<dyn Future<Output = Result<Observation>> + Send + 'a>>;
trait ErasedAdapter: Send + Sync {
    fn mode(&self) -> Mode;
    fn max_input_bytes(&self) -> usize;
    fn capabilities(&self) -> Capabilities;
    fn execute<'a>(
        &'a self,
        request: &'a Dispatch<'a>,
        key: &'a str,
        previous: Option<&'a Private>,
    ) -> Pending<'a>;
    fn observe<'a>(
        &'a self,
        request: &'a Dispatch<'a>,
        continuation: Option<&'a Private>,
    ) -> Pending<'a>;
}
impl<A: Adapter> ErasedAdapter for A {
    fn mode(&self) -> Mode {
        Adapter::mode(self)
    }
    fn max_input_bytes(&self) -> usize {
        Adapter::max_input_bytes(self)
    }
    fn capabilities(&self) -> Capabilities {
        Adapter::capabilities(self)
    }
    fn execute<'a>(
        &'a self,
        request: &'a Dispatch<'a>,
        key: &'a str,
        previous: Option<&'a Private>,
    ) -> Pending<'a> {
        Box::pin(Adapter::execute(self, request, key, previous))
    }
    fn observe<'a>(
        &'a self,
        request: &'a Dispatch<'a>,
        continuation: Option<&'a Private>,
    ) -> Pending<'a> {
        Box::pin(Adapter::observe(self, request, continuation))
    }
}

/// A current payment fact, created only by the executor. No provider raw data.
#[derive(Clone, Serialize)]
pub struct Outcome(Snapshot);

#[derive(Clone, Serialize, Deserialize)]
struct Snapshot {
    request_digest: [u8; 32],
    satisfies_terms: bool,
    cost_violation: bool,
    sandbox: bool,
    id: [u8; 32],
    adapter: String,
    operation: String,
    binding: Binding,
    state: State,
    valid_until: Option<u64>,
    receipt: Option<[u8; 32]>,
}
impl fmt::Debug for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Outcome")
            .field("state", &self.0.state)
            .finish_non_exhaustive()
    }
}
impl Outcome {
    /// Whether the adapter established the exact financial terms, including
    /// amount evidence. This does not itself grant access or change policy.
    pub fn satisfies_terms(&self) -> bool {
        self.0.satisfies_terms
    }
    /// The environment that established this fact. Sandbox facts are not proofs.
    pub fn mode(&self) -> Mode {
        if self.0.sandbox {
            Mode::Sandbox
        } else {
            Mode::Production
        }
    }
    pub(crate) fn matches_request(&self, request: &Instruction, digest: [u8; 32]) -> bool {
        self.0.satisfies_terms
            && self.0.request_digest == digest
            && self.0.id == request.id
            && self.0.binding == request.binding
    }
    /// Current external state, without any implicit commercial interpretation.
    pub fn state(&self) -> State {
        self.0.state
    }
    /// Exact adapter that established the fact.
    pub fn adapter(&self) -> &str {
        &self.0.adapter
    }
    /// Exact operation whose result was observed.
    pub fn operation(&self) -> &str {
        &self.0.operation
    }
    /// Validate a caller-selected fact and reduce it to gate metadata.
    /// The caller chooses the entitlement lifetime; provider validity caps it.
    pub fn evidence(
        &self,
        binding: &Binding,
        state: State,
        now: u64,
        until: u64,
    ) -> Result<Evidence> {
        if self.0.cost_violation
            || self.0.sandbox
            || &self.0.binding != binding
            || self.0.state != state
            || matches!(
                state,
                State::Pending | State::Unknown | State::RequiresAction | State::Failed
            )
        {
            return Err(Error::Rejected);
        }
        let until = self.0.valid_until.map_or(until, |limit| until.min(limit));
        if until <= now {
            return Err(Error::Expired);
        }
        Ok(Evidence {
            provider: self.0.adapter.clone(),
            binding: binding.clone(),
            valid_until: until,
            operation: self.0.receipt.ok_or(Error::Rejected)?,
        })
    }
}

/// Minimal in-process gate evidence. Never deserialize this from a client.
pub struct Evidence {
    provider: String,
    binding: Binding,
    valid_until: u64,
    operation: [u8; 32],
}
impl Evidence {
    /// Check member, service and action again at the next boundary.
    pub fn in_context(&self, binding: &Binding, now: u64) -> Result<(&str, u64)> {
        if &self.binding != binding {
            return Err(Error::Rejected);
        }
        if now >= self.valid_until {
            return Err(Error::Expired);
        }
        Ok((&self.provider, self.valid_until))
    }
    /// Opaque single-use marker for the gate host's atomic result/claim commit.
    pub fn claim(&self) -> &[u8; 32] {
        &self.operation
    }
}

/// Execution result with a separate, nonserializable member-only handover.
pub struct Execution {
    /// Caller state failed to commit the provider result. The returned outcome
    /// is Unknown and cannot prove success; reconcile using private diagnostics
    /// and preserved fee reports, never repeat the payment under a fresh ID.
    pub commit_error: Option<Error>,
    /// Reserved fee exposure and any actual charge established by the adapter.
    pub cost: Reservation,
    /// Minimal current payment fact for the caller.
    pub outcome: Outcome,
    /// Deliver only to the member authenticated by the original binding.
    pub member_action: Option<Private>,
    /// Requested private responses; empty for minimal output or a journal replay.
    pub upstream: Vec<ProviderReply>,
}

mod journal;
pub use journal::{Journal, MemoryJournal, Record};
mod engine;
pub use engine::{AccountIdentity, Payments, account_identity};

pub(crate) fn identifier(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn validate_binding(binding: &Binding) -> Result<()> {
    for value in [&binding.scope, &binding.subject, &binding.action] {
        identifier(value)?;
    }
    Ok(())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "tests/payments.rs"]
pub(crate) mod tests;
