CREATE TABLE coordinator_refusals (
    id TEXT PRIMARY KEY,
    engagement_id TEXT NOT NULL REFERENCES coordinator_engagements(id),
    digest TEXT NOT NULL,
    receipt TEXT NOT NULL CHECK(json_valid(receipt)),
    refused_at INTEGER NOT NULL
) STRICT;
