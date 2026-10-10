use super::*;

fn record(revision: u64) -> Record {
    serde_json::from_value(serde_json::json!({"fingerprint":vec![1;32],"owner":vec![3;32],"outcome":{"id":vec![2;32],"adapter":"test","operation":"collect","binding":{"scope":"scope","subject":"member","action":"join"},"state":"unknown","valid_until":null,"receipt":null},"continuation":null,"revision":revision})).unwrap()
}

#[tokio::test]
async fn durable_journal_survives_reopen_and_enforces_namespace_and_revision() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("file://{}", dir.path().join("journal.db").display());
    let db = crlt::Db::open(crlt::Config::new(&url, "")).await.unwrap();
    let migrations = [crlt::Migration {
        version: 1,
        name: "payments",
        sql: SCHEMA,
    }];
    db.migrate(&migrations).await.unwrap();
    let journal = LibsqlJournal::new(&db, "payments").unwrap();
    journal.bind(&[7; 32]).await.unwrap();
    journal.bind(&[7; 32]).await.unwrap();
    assert_eq!(
        journal.bind(&[8; 32]).await.unwrap_err(),
        Error::KeyMismatch
    );
    assert!(journal.get("key").await.unwrap().is_none());
    assert!(journal.begin("key", &record(0)).await.unwrap().is_none());
    assert_eq!(
        journal
            .begin("key", &record(99))
            .await
            .unwrap()
            .unwrap()
            .revision,
        0
    );
    journal.replace("key", 0, &record(1)).await.unwrap();
    assert_eq!(
        journal.replace("key", 0, &record(2)).await.unwrap_err(),
        Error::Conflict
    );
    assert_eq!(
        journal.replace("missing", 0, &record(1)).await.unwrap_err(),
        Error::Conflict
    );
    assert_eq!(
        journal
            .replace("key", u64::MAX, &record(2))
            .await
            .unwrap_err(),
        Error::Storage
    );
    assert_eq!(
        journal
            .replace("key", 1, &record(u64::MAX))
            .await
            .unwrap_err(),
        Error::Storage
    );
    assert!(
        LibsqlJournal::new(&db, "another")
            .unwrap()
            .get("key")
            .await
            .unwrap()
            .is_none()
    );
    assert!(LibsqlJournal::new(&db, "").is_err());
    drop(journal);
    drop(db);
    let db = crlt::Db::open(crlt::Config::new(url, "")).await.unwrap();
    db.migrate(&migrations).await.unwrap();
    let journal = LibsqlJournal::new(&db, "payments").unwrap();
    assert_eq!(journal.get("key").await.unwrap().unwrap().revision, 1);
    let scope = db.community("payments").unwrap();
    scope
        .execute(
            "UPDATE cpmt_operations SET record = ?1 WHERE operation = ?2",
            params!["corrupt", "key"],
        )
        .await
        .unwrap();
    assert!(matches!(journal.get("key").await, Err(Error::Storage)));
    assert!(matches!(
        journal.begin("key", &record(0)).await,
        Err(Error::Storage)
    ));
}

#[tokio::test]
async fn missing_schema_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("file://{}", dir.path().join("journal.db").display());
    let db = crlt::Db::open(crlt::Config::new(url, "")).await.unwrap();
    let journal = LibsqlJournal::new(&db, "payments").unwrap();
    assert!(matches!(journal.get("key").await, Err(Error::Storage)));
    assert!(matches!(
        journal.begin("key", &record(0)).await,
        Err(Error::Storage)
    ));
    assert_eq!(
        journal.replace("key", 0, &record(1)).await.unwrap_err(),
        Error::Storage
    );
}

#[tokio::test]
async fn receipt_ownership_and_result_commit_are_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("file://{}", dir.path().join("journal.db").display());
    let db = crlt::Db::open(crlt::Config::new(url, "")).await.unwrap();
    db.migrate(&[crlt::Migration::new(1, "payments", SCHEMA)])
        .await
        .unwrap();
    let journal = LibsqlJournal::new(&db, "payments").unwrap();
    for key in ["first", "second", "third"] {
        journal.begin(key, &record(0)).await.unwrap();
    }
    let mut value = serde_json::to_value(record(1)).unwrap();
    value["outcome"]["receipt"] = serde_json::json!(vec![8; 32]);
    let claimed: Record = serde_json::from_value(value.clone()).unwrap();
    journal.replace("first", 0, &claimed).await.unwrap();
    journal.replace("second", 0, &claimed).await.unwrap();
    value["owner"] = serde_json::json!(vec![9; 32]);
    let foreign: Record = serde_json::from_value(value).unwrap();
    assert_eq!(
        journal.replace("third", 0, &foreign).await.unwrap_err(),
        Error::Conflict
    );
    assert_eq!(journal.get("third").await.unwrap().unwrap().revision, 0);
    let mut value = serde_json::to_value(record(1)).unwrap();
    value["outcome"]["receipt"] = serde_json::json!(vec![7; 32]);
    let another: Record = serde_json::from_value(value).unwrap();
    assert_eq!(
        journal.replace("absent", 0, &another).await.unwrap_err(),
        Error::Conflict
    );
    assert!(
        db.community("payments")
            .unwrap()
            .query(
                "SELECT owner FROM cpmt_receipts WHERE receipt = ?1",
                [crate::hex(&[7; 32])]
            )
            .await
            .unwrap()
            .is_empty()
    );
}
