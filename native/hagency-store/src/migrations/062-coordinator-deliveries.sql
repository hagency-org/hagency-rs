-- Delivered approvals must be visible before Matrix admission or reservation.
CREATE TABLE coordinator_deliveries (
    id TEXT PRIMARY KEY,
    engagement_id TEXT NOT NULL REFERENCES coordinator_engagements(id),
    digest TEXT NOT NULL,
    command TEXT NOT NULL CHECK(json_valid(command)),
    definition TEXT NOT NULL CHECK(json_valid(definition)),
    state TEXT NOT NULL CHECK(state IN ('pending','applied','refused')),
    reason TEXT,
    agent_id TEXT,
    received_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
CREATE INDEX coordinator_deliveries_engagement ON coordinator_deliveries(engagement_id,id);
