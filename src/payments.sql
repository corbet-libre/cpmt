CREATE TABLE cpmt_operations (
    community_id TEXT NOT NULL,
    operation TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    record TEXT NOT NULL,
    PRIMARY KEY (community_id, operation)
) WITHOUT ROWID;
CREATE TABLE cpmt_receipts (
    community_id TEXT NOT NULL,
    receipt TEXT NOT NULL,
    owner TEXT NOT NULL,
    PRIMARY KEY (community_id, receipt)
) WITHOUT ROWID;
