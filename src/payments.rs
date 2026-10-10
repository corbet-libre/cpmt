//! Shared execution contract. Adapters own provider behavior; callers own policy.
use crate::cost::{self, Dispatch, DispatchPermit, Mode, Price, Quote, Reservation};
use crate::details::{Disclosure, ProviderReply};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{
    collections::BTreeMap,
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use zeroize::{Zeroize, Zeroizing};

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
        if self.0.sandbox
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
    /// Reserved fee exposure and any actual charge established by the adapter.
    pub cost: Reservation,
    /// Minimal current payment fact for the caller.
    pub outcome: Outcome,
    /// Deliver only to the member authenticated by the original binding.
    pub member_action: Option<Private>,
    /// Requested private responses; empty for minimal output or a journal replay.
    pub upstream: Vec<ProviderReply>,
}

/// Minimal journal entry. Provider continuation remains inside the storage owner.
#[derive(Clone, Serialize, Deserialize)]
pub struct Record {
    cost: Reservation,
    fingerprint: [u8; 32],
    owner: [u8; 32],
    outcome: Snapshot,
    continuation: Option<Vec<u8>>,
    pub(crate) revision: u64,
}
impl Record {
    /// The host atomically reserves this fee with `begin`, under its configured
    /// budget scope. Pending/unknown records retain maximum exposure on `replace`.
    pub fn cost(&self) -> &Reservation {
        &self.cost
    }
    /// Current state for the host's atomic exposure reconciliation.
    pub fn state(&self) -> State {
        self.outcome.state
    }
    /// Keyed receipt and context identities for the journal's atomic reservation.
    /// Equal receipts may recur only under the same context. Neither is raw input.
    pub fn receipt(&self) -> Option<([u8; 32], [u8; 32])> {
        self.outcome.receipt.map(|receipt| (receipt, self.owner))
    }
    /// Revision to persist alongside this opaque record for compare-and-swap.
    pub fn revision(&self) -> u64 {
        self.revision
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

/// Caller-owned atomic operational state. The library implements no persistence.
/// Production hosts must preserve it across restarts. It receives only minimal
/// operational records, never raw requests or upstream responses. Never log them.
pub trait Journal: Send + Sync {
    /// Atomically bind the namespace to a stable key fingerprint; reject changes.
    fn bind(&self, key_id: &[u8; 32]) -> impl Future<Output = Result<()>> + Send;
    /// Load an existing entry without changing it.
    fn get(&self, key: &str) -> impl Future<Output = Result<Option<Record>>> + Send;
    /// Atomically reserve this operation AND its maximum fee under the host's
    /// configured budget. Refuse insufficient exposure before inserting anything.
    /// Return an existing operation without reserving twice. Production hosts
    /// must preserve this transaction across crashes and concurrent callers.
    fn begin(
        &self,
        key: &str,
        record: &Record,
    ) -> impl Future<Output = Result<Option<Record>>> + Send;
    /// Atomically replace the expected revision and reserve any receipt for its
    /// context. Another context cannot claim it. Conflicts leave data unchanged.
    fn replace(
        &self,
        key: &str,
        expected_revision: u64,
        record: &Record,
    ) -> impl Future<Output = Result<()>> + Send;
}

/// Real volatile journal for disposable use and conformance tests.
/// Production payment execution requires a durable `Journal` implementation.
#[derive(Default, Clone)]
pub struct MemoryJournal(Arc<Mutex<MemoryState>>);

#[derive(Default)]
struct MemoryState {
    budget: Option<cost::Amount>,
    key_id: Option<[u8; 32]>,
    records: BTreeMap<String, Record>,
    receipts: BTreeMap<[u8; 32], [u8; 32]>,
}
impl MemoryJournal {
    /// Volatile budget accounting for fixtures or disposable single-process use.
    /// Restart guarantees require a caller-owned durable state implementation.
    pub fn with_budget(budget: cost::Amount) -> Self {
        Self(Arc::new(Mutex::new(MemoryState {
            budget: Some(budget),
            ..MemoryState::default()
        })))
    }
}
impl Journal for MemoryJournal {
    async fn bind(&self, key_id: &[u8; 32]) -> Result<()> {
        let mut state = self.0.lock().map_err(|_| Error::Storage)?;
        if state.key_id.is_some_and(|previous| previous != *key_id) {
            return Err(Error::KeyMismatch);
        }
        state.key_id = Some(*key_id);
        Ok(())
    }
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
        if let Some(budget) = &state.budget {
            if record.cost.unit != budget.unit.code || record.cost.scale != budget.unit.scale {
                return Err(Error::Cost);
            }
            let exposure = state
                .records
                .values()
                .try_fold(record.cost.maximum, |total, r| {
                    let charge = if matches!(
                        r.outcome.state,
                        State::Unknown | State::Pending | State::RequiresAction
                    ) {
                        r.cost.maximum
                    } else {
                        r.cost.actual.unwrap_or(r.cost.maximum)
                    };
                    total.checked_add(charge).ok_or(Error::Cost)
                })?;
            if exposure > budget.atoms {
                return Err(Error::Cost);
            }
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
    initialized: AtomicBool,
    adapters: BTreeMap<String, Arc<dyn ErasedAdapter>>,
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
            initialized: AtomicBool::new(false),
            adapters: BTreeMap::new(),
        })
    }
    /// Register one trusted adapter. Existing identities cannot be replaced.
    pub fn register<A: Adapter + 'static>(&mut self, adapter: Arc<A>) -> Result<()> {
        let capabilities = Adapter::capabilities(adapter.as_ref());
        identifier(&capabilities.id)?;
        if capabilities.operations.is_empty() || Adapter::max_input_bytes(adapter.as_ref()) == 0 {
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
    /// Construct an inert preview from a trusted quote or configured tariff.
    /// No pricing or provider endpoint is called. Unknown prices remain unknown.
    pub fn preview(
        &self,
        request: &Instruction,
        price: Price,
        source: String,
        valid_until: u64,
    ) -> Result<Quote> {
        let adapter = self
            .adapters
            .get(&request.adapter)
            .ok_or(Error::Unsupported)?;
        self.validate_request(request, adapter.as_ref())?;
        Ok(Quote {
            price,
            source,
            valid_until: valid_until.min(request.expires_at),
            binding: cost::binding(request, adapter.mode()),
        })
    }
    fn validate_request(&self, request: &Instruction, adapter: &dyn ErasedAdapter) -> Result<()> {
        if !adapter
            .capabilities()
            .operations
            .contains(&request.operation)
        {
            return Err(Error::Unsupported);
        }
        if request
            .terms
            .expose()
            .len()
            .saturating_add(request.evidence.expose().len())
            > adapter.max_input_bytes()
        {
            return Err(Error::Invalid);
        }
        validate_binding(&request.binding)?;
        if request.id == [0; 32] || request.expires_at == 0 {
            return Err(Error::Invalid);
        }
        if request.kind == Action::Observe
            && (request.parent.is_none()
                || !request.terms.expose().is_empty()
                || !request.evidence.expose().is_empty())
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    /// Execute at most once per operation ID, even across concurrent calls/restarts.
    /// Lost/uncertain outcomes require `observe`, never a second execution.
    pub async fn dispatch(
        &self,
        request: &Instruction,
        permit: DispatchPermit<String>,
        now: u64,
    ) -> Result<Execution> {
        self.dispatch_with_output(request, permit, now, Disclosure::Minimal)
            .await
    }
    /// Execute with explicit transient disclosure. A replay cannot reconstruct
    /// discarded provider data and returns only the caller's retained snapshot.
    pub async fn dispatch_with_output(
        &self,
        request: &Instruction,
        permit: DispatchPermit<String>,
        now: u64,
        disclosure: Disclosure,
    ) -> Result<Execution> {
        let adapter = self
            .adapters
            .get(&request.adapter)
            .ok_or(Error::Unsupported)?;
        self.validate_request(request, adapter.as_ref())?;
        cost::validate(
            request,
            &adapter.capabilities().id,
            adapter.mode(),
            &permit,
            now,
        )?;
        let key = self.operation_key(&request.binding, &request.id).await?;
        let fingerprint = self.instruction_digest(request, adapter.mode());
        let initial = Record {
            cost: Reservation::from_permit(&permit)?,
            fingerprint,
            owner: self.mac(
                &serde_json::to_vec(&("cpmt.owner.v2", &request.binding))
                    .expect("fixed payment fields serialize to JSON"),
            ),
            outcome: Snapshot {
                request_digest: fingerprint,
                satisfies_terms: false,
                sandbox: adapter.mode() == Mode::Sandbox,
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
                cost: previous.cost.clone(),
                outcome: Outcome(previous.outcome.clone()),
                member_action: None,
                upstream: Vec::new(),
            });
        }
        let parent = if let Some(id) = &request.parent {
            let parent_key = self.operation_key(&request.binding, id).await?;
            let record = self.load(&parent_key, &request.binding).await?;
            if record.outcome.adapter != request.adapter
                || id == &request.id
                || record.outcome.sandbox != (adapter.mode() == Mode::Sandbox)
                || (request.kind == Action::Observe
                    && record.outcome.operation != request.operation)
            {
                return Err(Error::Rejected);
            }
            Some(Private::new(
                record.continuation.clone().ok_or(Error::Rejected)?,
            ))
        } else {
            None
        };
        // Cancellation, a crash, or an adapter error leaves the uncertain marker.
        let authorized = Dispatch::new(request, permit, now)?;
        let observation = match request.kind {
            Action::Execute => adapter.execute(&authorized, &key, parent.as_ref()).await?,
            Action::Observe => adapter.observe(&authorized, parent.as_ref()).await?,
        };
        self.finish(&key, initial, observation, disclosure).await
    }
    /// Prepare a separately costed read of an authenticated parent operation.
    /// This performs no provider I/O. Execute the returned instruction with a new
    /// quote/permit; its fresh ID reserves observation fees exactly once.
    pub async fn observation(
        &self,
        id: &[u8; 32],
        binding: &Binding,
        read_id: [u8; 32],
        expires_at: u64,
    ) -> Result<Instruction> {
        let key = self.operation_key(binding, id).await?;
        let previous = self.load(&key, binding).await?;
        // No reference means no read-only reconciliation is possible. In
        // particular, a poll must not revise the marker of an in-flight dispatch.
        if previous.continuation.is_none() {
            return Err(Error::Unsupported);
        }
        Ok(Instruction {
            kind: Action::Observe,
            id: read_id,
            parent: Some(*id),
            adapter: previous.outcome.adapter.clone(),
            operation: previous.outcome.operation.clone(),
            binding: binding.clone(),
            expires_at,
            terms: Private::new(Vec::new()),
            evidence: Private::new(Vec::new()),
        })
    }
    /// Retrieve the latest journaled fact, with no network call or raw provider data.
    pub async fn outcome(&self, id: &[u8; 32], binding: &Binding) -> Result<Outcome> {
        let key = self.operation_key(binding, id).await?;
        Ok(Outcome(self.load(&key, binding).await?.outcome.clone()))
    }
    /// Erase provider continuation when the caller no longer needs reconciliation.
    /// Keep the minimal operation marker to prevent accidental re-execution.
    pub async fn forget(&self, id: &[u8; 32], binding: &Binding) -> Result<()> {
        let key = self.operation_key(binding, id).await?;
        let mut record = self.load(&key, binding).await?;
        let revision = record.revision;
        // An unfinished dispatch can still receive a reference. Do not report
        // successful erasure while that write is outstanding.
        if revision == 0 {
            return Err(Error::Conflict);
        }
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
    async fn operation_key(&self, binding: &Binding, id: &[u8; 32]) -> Result<String> {
        validate_binding(binding)?;
        if !self.initialized.load(Ordering::Acquire) {
            self.journal.bind(&self.mac(b"cpmt.journal-key.v2")).await?;
            self.initialized.store(true, Ordering::Release);
        }
        let bytes = serde_json::to_vec(&("cpmt.operation.v2", &binding.scope, id))
            .expect("fixed payment fields serialize to JSON");
        Ok(crate::hex(&self.mac(&bytes)))
    }
    async fn finish(
        &self,
        key: &str,
        mut record: Record,
        observation: Observation,
        disclosure: Disclosure,
    ) -> Result<Execution> {
        let revision = record.revision;
        record.revision = revision.checked_add(1).ok_or(Error::Storage)?;
        record.outcome.state = observation.state;
        record.outcome.valid_until = observation.valid_until;
        record.outcome.satisfies_terms = observation.satisfies_terms;
        record.cost.report(observation.actual_cost.as_ref())?;
        // This fresh immutable operation has no prior continuation to overwrite.
        record.continuation = observation.continuation.map(|p| p.expose().to_vec());
        record.outcome.receipt = observation.claim.as_ref().map(|claim| {
            let bytes = Zeroizing::new(
                serde_json::to_vec(&("cpmt.receipt.v2", claim.expose()))
                    .expect("fixed payment fields serialize to JSON"),
            );
            self.mac(&bytes)
        });
        self.journal.replace(key, revision, &record).await?;
        Ok(Execution {
            cost: record.cost.clone(),
            outcome: Outcome(record.outcome.clone()),
            member_action: observation.member_action,
            upstream: match disclosure {
                Disclosure::Minimal => Vec::new(),
                Disclosure::Complete => observation.upstream,
            },
        })
    }
    pub(crate) fn instruction_digest(&self, request: &Instruction, mode: Mode) -> [u8; 32] {
        // The journal retains a keyed digest, never the raw terms or member evidence.
        let bytes = Zeroizing::new(
            serde_json::to_vec(&(
                "cpmt.instruction.v2",
                request.kind,
                mode == Mode::Sandbox,
                &request.adapter,
                &request.operation,
                &request.binding,
                request.parent,
                request.expires_at,
                request.terms.expose(),
                request.evidence.expose(),
            ))
            .expect("fixed payment fields serialize to JSON"),
        );
        self.mac(&bytes)
    }
    fn mac(&self, bytes: &[u8]) -> [u8; 32] {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.key.as_ref())
            .expect("HMAC accepts the fixed 32-byte key");
        mac.update(bytes);
        mac.finalize().into_bytes().into()
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
pub(crate) mod tests;
