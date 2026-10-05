-- Local owner migration receipts are provenance, not coordinator approvals.
CREATE TABLE coordinator_migrations (
    id TEXT PRIMARY KEY,
    engagement_id TEXT NOT NULL REFERENCES coordinator_engagements(id),
    digest TEXT NOT NULL,
    receipt TEXT NOT NULL CHECK(json_valid(receipt)),
    accepted_at INTEGER NOT NULL
) STRICT;
