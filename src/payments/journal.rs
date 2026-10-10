//! Caller-owned atomic operational state and its volatile conformance implementation.
use super::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use zeroize::Zeroize;

/// Minimal journal entry. Provider continuation remains inside the storage owner.
#[derive(Clone, Serialize, Deserialize)]
pub struct Record {
    pub(super) cost: Reservation,
    pub(super) fingerprint: [u8; 32],
    pub(super) owner: [u8; 32],
    pub(super) outcome: Snapshot,
    pub(super) continuation: Option<Vec<u8>>,
    pub(crate) revision: u64,
}
impl Record {
    /// Project an existing trusted record for hosts that permit only opaque
    /// execution metadata at rest. The original continuation remains in RAM;
    /// all request/receipt bindings, facts, fees and revision remain unchanged.
    /// Reopening this projection cannot observe/refund a provider object unless
    /// the host independently retained or re-established its actual authority.
    pub fn without_continuation(&self) -> Self {
        Self {
            cost: self.cost.clone(),
            fingerprint: self.fingerprint,
            owner: self.owner,
            outcome: self.outcome.clone(),
            continuation: None,
            revision: self.revision,
        }
    }
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
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
    /// Check the effective quote/request deadline with the fresh trusted clock
    /// inside the reservation decision. Existing charge violations block new
    /// reservations in the affected host budget scope until reconciled.
    fn begin(
        &self,
        key: &str,
        record: &Record,
        deadline: u64,
        clock: &dyn cost::Clock,
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
pub struct MemoryJournal(pub(super) Arc<Mutex<MemoryState>>);

#[derive(Default)]
pub(super) struct MemoryState {
    pub(super) budget: Option<cost::Amount>,
    pub(super) key_id: Option<[u8; 32]>,
    pub(super) records: BTreeMap<String, Record>,
    pub(super) receipts: BTreeMap<[u8; 32], [u8; 32]>,
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
    async fn begin(
        &self,
        key: &str,
        record: &Record,
        deadline: u64,
        clock: &dyn cost::Clock,
    ) -> Result<Option<Record>> {
        let mut state = self.0.lock().map_err(|_| Error::Storage)?;
        if let Some(record) = state.records.get(key) {
            return Ok(Some(record.clone()));
        }
        if clock.now() >= deadline {
            return Err(Error::Expired);
        }
        if state.records.values().any(|r| r.cost.violation.is_some()) {
            return Err(Error::Cost);
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
