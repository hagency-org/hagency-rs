CREATE TABLE coordinator_delegations (
    engagement_id TEXT NOT NULL REFERENCES coordinator_engagements(id),
    revision INTEGER NOT NULL,
    digest TEXT NOT NULL,
    change TEXT NOT NULL CHECK(json_valid(change)),
    authority TEXT NOT NULL CHECK(json_valid(authority)),
    accepted_at INTEGER NOT NULL,
    PRIMARY KEY(engagement_id,revision)
) STRICT;
