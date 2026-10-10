//! Durable payment journal inside the payment boundary, using the shared crlt pool.
use crate::payments::{Error, Journal, Record, Result};
use crlt::params;
use zeroize::Zeroizing;

/// Append to the composition root's complete ordered migration list.
pub const SCHEMA: &str = include_str!("payments.sql");

/// A journal namespace owned exclusively by the payment service.
/// Do not expose its database, records or provider references to gate consumers.
#[derive(Clone)]
pub struct LibsqlJournal(crlt::Community);

impl LibsqlJournal {
    /// Apply [`SCHEMA`] before constructing or reopening this journal.
    pub fn new(db: &crlt::Db, namespace: &str) -> Result<Self> {
        Ok(Self(db.community(namespace).map_err(|_| Error::Storage)?))
    }
}

const LOAD: &str = "SELECT record FROM cpmt_operations WHERE operation = ?1";

fn decode(rows: Vec<crlt::Row>) -> Result<Option<Record>> {
    rows.first()
        .map(|row| {
            serde_json::from_str(row.get_str(0).map_err(|_| Error::Storage)?)
                .map_err(|_| Error::Storage)
        })
        .transpose()
}

impl Journal for LibsqlJournal {
    async fn bind(&self, key_id: &[u8; 32]) -> Result<()> {
        let mut tx = self.0.tx().await.map_err(|_| Error::Storage)?;
        let rows = tx
            .query("SELECT key_id FROM cpmt_identity", ())
            .await
            .map_err(|_| Error::Storage)?;
        let key_id = crate::hex(key_id);
        if let Some(row) = rows.first() {
            if row.get_str(0).map_err(|_| Error::Storage)? != key_id {
                return Err(Error::KeyMismatch);
            }
        } else {
            tx.execute("INSERT INTO cpmt_identity (key_id) VALUES (?1)", [key_id])
                .await
                .map_err(|_| Error::Storage)?;
        }
        tx.commit().await.map_err(|_| Error::Storage)?;
        Ok(())
    }
    async fn get(&self, key: &str) -> Result<Option<Record>> {
        decode(
            self.0
                .query(LOAD, [key])
                .await
                .map_err(|_| Error::Storage)?,
        )
    }

    async fn begin(&self, key: &str, record: &Record) -> Result<Option<Record>> {
        let mut tx = self.0.tx().await.map_err(|_| Error::Storage)?;
        let previous = decode(tx.query(LOAD, [key]).await.map_err(|_| Error::Storage)?)?;
        if previous.is_none() {
            let data = Zeroizing::new(serde_json::to_string(record).map_err(|_| Error::Storage)?);
            let revision = i64::try_from(record.revision).map_err(|_| Error::Storage)?;
            tx.execute(
                "INSERT INTO cpmt_operations (operation, revision, record) VALUES (?1, ?2, ?3)",
                params![key, revision, data.as_str()],
            )
            .await
            .map_err(|_| Error::Storage)?;
        }
        tx.commit().await.map_err(|_| Error::Storage)?;
        Ok(previous)
    }

    async fn replace(&self, key: &str, expected_revision: u64, record: &Record) -> Result<()> {
        let data = Zeroizing::new(serde_json::to_string(record).map_err(|_| Error::Storage)?);
        let expected = i64::try_from(expected_revision).map_err(|_| Error::Storage)?;
        let revision = i64::try_from(record.revision).map_err(|_| Error::Storage)?;
        let mut tx = self.0.tx().await.map_err(|_| Error::Storage)?;
        if let Some((receipt, owner)) = record.receipt() {
            let receipt = crate::hex(&receipt);
            let owner = crate::hex(&owner);
            let rows = tx
                .query(
                    "SELECT owner FROM cpmt_receipts WHERE receipt = ?1",
                    [receipt.as_str()],
                )
                .await
                .map_err(|_| Error::Storage)?;
            if let Some(row) = rows.first() {
                if row.get_str(0).map_err(|_| Error::Storage)? != owner {
                    return Err(Error::Conflict);
                }
            } else {
                tx.execute(
                    "INSERT INTO cpmt_receipts (receipt, owner) VALUES (?1, ?2)",
                    params![receipt, owner],
                )
                .await
                .map_err(|_| Error::Storage)?;
            }
        }
        let count = tx.execute("UPDATE cpmt_operations SET revision = ?1, record = ?2 WHERE operation = ?3 AND revision = ?4", params![revision, data.as_str(), key, expected])
            .await.map_err(|_| Error::Storage)?;
        if count != 1 {
            return Err(Error::Conflict);
        }
        tx.commit().await.map_err(|_| Error::Storage)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/storage.rs"]
mod tests;
