//! Cost-authorized execution and immutable observations.
use super::*;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use zeroize::Zeroize;

/// Opaque equality commitment for one caller-configured account namespace.
/// It establishes no payment, member identity or financial authority.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AccountIdentity([u8; 32]);
impl AccountIdentity {
    /// Nonsecret equality commitment, suitable for caller-owned stable binding.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Bind the actual scope, provider-account identity, endpoint and mode through
/// the executor's existing maintained HMAC engine and unambiguous JSON tuple.
/// Pass a temporary key copy: it is wiped even when configuration is invalid.
/// Replaceable credentials are deliberately absent from this equality contract.
pub fn account_identity(
    key_copy: &mut [u8; 32],
    scope: &str,
    provider: &str,
    account: &str,
    endpoint: &str,
    mode: Mode,
) -> Result<AccountIdentity> {
    let key = Zeroizing::new(*key_copy);
    key_copy.zeroize();
    for value in [scope, provider, account] {
        identifier(value)?;
    }
    if *key == [0; 32]
        || endpoint.trim().is_empty()
        || endpoint.len() > 2048
        || endpoint.chars().any(char::is_control)
        || mode == Mode::Simulation
    {
        return Err(Error::Invalid);
    }
    let mode = match mode {
        Mode::Production => "production",
        Mode::Sandbox => "sandbox",
        Mode::Simulation => unreachable!(),
    };
    let bytes = serde_json::to_vec(&(
        "cpmt.account-identity.v1",
        scope,
        provider,
        account,
        endpoint,
        mode,
    ))
    .map_err(|_| Error::Invalid)?;
    Ok(AccountIdentity(key_mac(&key, &bytes)))
}

fn key_mac(key: &[u8; 32], bytes: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts the fixed 32-byte key");
    mac.update(bytes);
    mac.finalize().into_bytes().into()
}

/// Registry and executor. No provider ordering, pricing, account balance or custody.
pub struct Payments<J> {
    journal: J,
    clock: Arc<dyn cost::Clock>,
    key: Zeroizing<[u8; 32]>,
    initialized: AtomicBool,
    pub(super) adapters: BTreeMap<String, Arc<dyn ErasedAdapter>>,
}
impl<J: Journal> Payments<J> {
    /// Use an injected, stable, random journal-binding key; never discover secrets.
    pub fn new(journal: J, key: [u8; 32], clock: Arc<dyn cost::Clock>) -> Result<Self> {
        if key == [0; 32] {
            return Err(Error::Invalid);
        }
        Ok(Self {
            journal,
            clock,
            key: Zeroizing::new(key),
            initialized: AtomicBool::new(false),
            adapters: BTreeMap::new(),
        })
    }
    pub(crate) fn current_time(&self, admission: u64) -> u64 {
        admission.max(self.clock.now())
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
            self.current_time(now),
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
                cost_violation: false,
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
        if let Some(previous) = self
            .journal
            .begin(
                &key,
                &initial,
                request.expires_at.min(permit.quote().valid_until),
                self.clock.as_ref(),
            )
            .await?
        {
            if previous.fingerprint != fingerprint {
                return Err(Error::Conflict);
            }
            return Ok(Execution {
                commit_error: None,
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
        let authorized = Dispatch::new(request, permit, now, self.clock.clone())?;
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
        record.outcome.cost_violation = record
            .cost
            .report(observation.actual_cost.as_ref())
            .is_err();
        record.outcome.satisfies_terms &= !record.outcome.cost_violation;
        // This fresh immutable operation has no prior continuation to overwrite.
        record.continuation = observation.continuation.map(|p| p.expose().to_vec());
        record.outcome.receipt = observation.claim.as_ref().map(|claim| {
            let bytes = Zeroizing::new(
                serde_json::to_vec(&("cpmt.receipt.v2", claim.expose()))
                    .expect("fixed payment fields serialize to JSON"),
            );
            self.mac(&bytes)
        });
        let commit_error = self.journal.replace(key, revision, &record).await.err();
        if commit_error.is_some() {
            record.outcome.state = State::Unknown;
            record.outcome.satisfies_terms = false;
            record.outcome.receipt = None;
        }
        Ok(Execution {
            commit_error,
            cost: record.cost.clone(),
            outcome: Outcome(record.outcome.clone()),
            member_action: if commit_error.is_none() {
                observation.member_action
            } else {
                None
            },
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
        key_mac(&self.key, bytes)
    }
}
