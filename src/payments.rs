//! Shared execution contract. Adapters own provider behavior; callers own policy.
use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Mutex},
};
use zeroize::{Zeroize, Zeroizing};

/// Uniform errors never embed provider responses, addresses or identifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
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
}
impl Observation {
    /// A minimal result without provider data.
    pub fn fact(state: State) -> Self {
        Self {
            state,
            valid_until: None,
            continuation: None,
            claim: None,
            member_action: None,
        }
    }
}

/// Adapter implementation is trusted code. Never construct one from member input.
#[async_trait]
pub trait Adapter: Send + Sync {
    /// Enumerate exactly the operations implemented by this configured adapter.
    fn capabilities(&self) -> Capabilities;
    /// Execute once. Any ambiguous transport/provider result must be `Unknown`.
    /// `key` is stable and must be used for upstream idempotency where supported.
    async fn execute(
        &self,
        request: &Instruction,
        key: &str,
        previous: Option<&Private>,
    ) -> Result<Observation>;
    /// Read/reconcile an existing operation; must never initiate a new charge.
    async fn observe(&self, operation: &str, continuation: Option<&Private>)
    -> Result<Observation>;
}

/// A current payment fact, created only by the executor. No provider raw data.
#[derive(Clone, Serialize)]
pub struct Outcome(Snapshot);

#[derive(Clone, Serialize, Deserialize)]
struct Snapshot {
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
        if &self.0.binding != binding
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
    /// Minimal current payment fact for the caller.
    pub outcome: Outcome,
    /// Deliver only to the member authenticated by the original binding.
    pub member_action: Option<Private>,
}

/// Minimal journal entry. Provider continuation remains inside the storage owner.
#[derive(Clone, Serialize, Deserialize)]
pub struct Record {
    fingerprint: [u8; 32],
    owner: [u8; 32],
    outcome: Snapshot,
    continuation: Option<Vec<u8>>,
    pub(crate) revision: u64,
}
impl Record {
    pub(crate) fn receipt(&self) -> Option<([u8; 32], [u8; 32])> {
        self.outcome.receipt.map(|receipt| (receipt, self.owner))
    }
}
impl Drop for Record {
    fn drop(&mut self) {
        if let Some(bytes) = &mut self.continuation {
            bytes.zeroize();
        }
    }
}
impl fmt::Debug for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Record([redacted])")
    }
}

/// Atomic journal. Production implementations must survive process restarts.
/// Key scope and raw records are private to the payment boundary. Never log them.
#[async_trait]
pub trait Journal: Send + Sync {
    /// Load an existing entry without changing it.
    async fn get(&self, key: &str) -> Result<Option<Record>>;
    /// Insert if absent and return the existing entry on conflict, atomically.
    async fn begin(&self, key: &str, record: &Record) -> Result<Option<Record>>;
    /// Atomically replace the expected revision and reserve any receipt for its
    /// context. Another context cannot claim it. Conflicts leave data unchanged.
    async fn replace(&self, key: &str, expected_revision: u64, record: &Record) -> Result<()>;
}

/// Real volatile journal for disposable use and conformance tests.
/// Production payment execution requires a durable `Journal` implementation.
#[derive(Default, Clone)]
pub struct MemoryJournal(Arc<Mutex<MemoryState>>);

#[derive(Default)]
struct MemoryState {
    records: BTreeMap<String, Record>,
    receipts: BTreeMap<[u8; 32], [u8; 32]>,
}
#[async_trait]
impl Journal for MemoryJournal {
    async fn get(&self, key: &str) -> Result<Option<Record>> {
        Ok(self
            .0
            .lock()
            .map_err(|_| Error::Storage)?
            .records
            .get(key)
            .cloned())
    }
    async fn begin(&self, key: &str, record: &Record) -> Result<Option<Record>> {
        let mut state = self.0.lock().map_err(|_| Error::Storage)?;
        if let Some(record) = state.records.get(key) {
            return Ok(Some(record.clone()));
        }
        state.records.insert(key.into(), record.clone());
        Ok(None)
    }
    async fn replace(&self, key: &str, expected_revision: u64, record: &Record) -> Result<()> {
        let mut state = self.0.lock().map_err(|_| Error::Storage)?;
        if state.records.get(key).map(|r| r.revision) != Some(expected_revision) {
            return Err(Error::Conflict);
        }
        if let Some((receipt, owner)) = record.receipt() {
            if state
                .receipts
                .get(&receipt)
                .is_some_and(|previous| previous != &owner)
            {
                return Err(Error::Conflict);
            }
            state.receipts.insert(receipt, owner);
        }
        state.records.insert(key.into(), record.clone());
        Ok(())
    }
}

/// Registry and executor. No provider ordering, pricing, account balance or custody.
pub struct Payments<J> {
    journal: J,
    key: Zeroizing<[u8; 32]>,
    adapters: BTreeMap<String, Arc<dyn Adapter>>,
}
impl<J: Journal> Payments<J> {
    /// Use an injected, stable, random journal-binding key; never discover secrets.
    pub fn new(journal: J, key: [u8; 32]) -> Result<Self> {
        if key == [0; 32] {
            return Err(Error::Invalid);
        }
        Ok(Self {
            journal,
            key: Zeroizing::new(key),
            adapters: BTreeMap::new(),
        })
    }
    /// Register one trusted adapter. Existing identities cannot be replaced.
    pub fn register(&mut self, adapter: Arc<dyn Adapter>) -> Result<()> {
        let capabilities = adapter.capabilities();
        identifier(&capabilities.id)?;
        if capabilities.operations.is_empty() {
            return Err(Error::Invalid);
        }
        for operation in &capabilities.operations {
            identifier(operation)?;
        }
        if self.adapters.contains_key(&capabilities.id) {
            return Err(Error::Conflict);
        }
        self.adapters.insert(capabilities.id, adapter);
        Ok(())
    }
    /// Describe registered adapter capabilities; caller policy selects among them.
    pub fn capabilities(&self) -> Vec<Capabilities> {
        self.adapters.values().map(|a| a.capabilities()).collect()
    }
    /// Execute at most once per operation ID, even across concurrent calls/restarts.
    /// Lost/uncertain outcomes require `observe`, never a second execution.
    pub async fn execute(&self, request: &Instruction, now: u64) -> Result<Execution> {
        let adapter = self
            .adapters
            .get(&request.adapter)
            .ok_or(Error::Unsupported)?;
        if !adapter
            .capabilities()
            .operations
            .contains(&request.operation)
        {
            return Err(Error::Unsupported);
        }
        validate_binding(&request.binding)?;
        if request.id == [0; 32] || request.expires_at == 0 {
            return Err(Error::Invalid);
        }
        if now >= request.expires_at {
            return Err(Error::Expired);
        }
        let key = self.operation_key(&request.binding, &request.id)?;
        // The journal retains a keyed digest, never the raw terms or member evidence.
        let bytes = Zeroizing::new(
            serde_json::to_vec(&(
                "cpmt.instruction.v2",
                &request.adapter,
                &request.operation,
                &request.binding,
                request.parent,
                request.expires_at,
                request.terms.expose(),
                request.evidence.expose(),
            ))
            .map_err(|_| Error::Invalid)?,
        );
        let fingerprint = self.mac(&bytes)?;
        let parent = if let Some(id) = &request.parent {
            let parent_key = self.operation_key(&request.binding, id)?;
            let record = self.load(&parent_key, &request.binding).await?;
            if record.outcome.adapter != request.adapter || id == &request.id {
                return Err(Error::Rejected);
            }
            Some(Private::new(
                record.continuation.clone().ok_or(Error::Rejected)?,
            ))
        } else {
            None
        };
        let initial = Record {
            fingerprint,
            owner: self.mac(
                &serde_json::to_vec(&("cpmt.owner.v2", &request.binding))
                    .map_err(|_| Error::Invalid)?,
            )?,
            outcome: Snapshot {
                id: request.id,
                adapter: request.adapter.clone(),
                operation: request.operation.clone(),
                binding: request.binding.clone(),
                state: State::Unknown,
                valid_until: None,
                receipt: None,
            },
            continuation: None,
            revision: 0,
        };
        if let Some(previous) = self.journal.begin(&key, &initial).await? {
            if previous.fingerprint != fingerprint {
                return Err(Error::Conflict);
            }
            return Ok(Execution {
                outcome: Outcome(previous.outcome.clone()),
                member_action: None,
            });
        }
        // Cancellation, a crash, or an adapter error leaves the uncertain marker.
        let observation = adapter.execute(request, &key, parent.as_ref()).await?;
        self.finish(&key, initial, observation).await
    }
    /// Reconcile using only the authenticated binding and operation ID.
    /// Original terms and raw member evidence can be discarded after execution.
    pub async fn observe(&self, id: &[u8; 32], binding: &Binding) -> Result<Execution> {
        let key = self.operation_key(binding, id)?;
        let previous = self.load(&key, binding).await?;
        let adapter = self
            .adapters
            .get(&previous.outcome.adapter)
            .ok_or(Error::Unsupported)?;
        let continuation = previous.continuation.clone().map(Private::new);
        let observation = adapter
            .observe(&previous.outcome.operation, continuation.as_ref())
            .await?;
        self.finish(&key, previous, observation).await
    }
    /// Retrieve the latest journaled fact, with no network call or raw provider data.
    pub async fn outcome(&self, id: &[u8; 32], binding: &Binding) -> Result<Outcome> {
        let key = self.operation_key(binding, id)?;
        Ok(Outcome(self.load(&key, binding).await?.outcome.clone()))
    }
    /// Erase provider continuation when the caller no longer needs reconciliation.
    /// Keep the minimal operation marker to prevent accidental re-execution.
    pub async fn forget(&self, id: &[u8; 32], binding: &Binding) -> Result<()> {
        let key = self.operation_key(binding, id)?;
        let mut record = self.load(&key, binding).await?;
        let revision = record.revision;
        record.revision = revision.checked_add(1).ok_or(Error::Storage)?;
        if let Some(mut bytes) = record.continuation.take() {
            bytes.zeroize();
        }
        self.journal.replace(&key, revision, &record).await
    }
    async fn load(&self, key: &str, binding: &Binding) -> Result<Record> {
        let record = self.journal.get(key).await?.ok_or(Error::Rejected)?;
        if &record.outcome.binding != binding {
            return Err(Error::Rejected);
        }
        Ok(record)
    }
    fn operation_key(&self, binding: &Binding, id: &[u8; 32]) -> Result<String> {
        validate_binding(binding)?;
        let bytes = serde_json::to_vec(&("cpmt.operation.v2", &binding.scope, id))
            .map_err(|_| Error::Invalid)?;
        Ok(crate::hex(&self.mac(&bytes)?))
    }
    async fn finish(
        &self,
        key: &str,
        mut record: Record,
        observation: Observation,
    ) -> Result<Execution> {
        let revision = record.revision;
        record.revision = revision.checked_add(1).ok_or(Error::Storage)?;
        record.outcome.state = observation.state;
        record.outcome.valid_until = observation.valid_until;
        // Every observation explicitly supplies what is still needed. None erases it.
        if let Some(mut bytes) = record.continuation.take() {
            bytes.zeroize();
        }
        record.continuation = observation.continuation.map(|p| p.expose().to_vec());
        record.outcome.receipt = observation
            .claim
            .as_ref()
            .map(|claim| {
                let bytes = Zeroizing::new(
                    serde_json::to_vec(&("cpmt.receipt.v2", claim.expose()))
                        .map_err(|_| Error::Invalid)?,
                );
                self.mac(&bytes)
            })
            .transpose()?;
        self.journal.replace(key, revision, &record).await?;
        Ok(Execution {
            outcome: Outcome(record.outcome.clone()),
            member_action: observation.member_action,
        })
    }
    fn mac(&self, bytes: &[u8]) -> Result<[u8; 32]> {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(self.key.as_ref()).map_err(|_| Error::Invalid)?;
        mac.update(bytes);
        Ok(mac.finalize().into_bytes().into())
    }
}

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
mod tests;
