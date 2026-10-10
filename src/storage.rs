//! Durable payment journal inside the payment boundary, using the shared crlt pool.
use crate::payments::{Error, Journal, Record, Result};
use async_trait::async_trait;
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

#[async_trait]
impl Journal for LibsqlJournal {
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
        let count = self.0.execute("UPDATE cpmt_operations SET revision = ?1, record = ?2 WHERE operation = ?3 AND revision = ?4", params![revision, data.as_str(), key, expected])
            .await.map_err(|_| Error::Storage)?;
        if count != 1 {
            return Err(Error::Conflict);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/storage.rs"]
mod tests;
