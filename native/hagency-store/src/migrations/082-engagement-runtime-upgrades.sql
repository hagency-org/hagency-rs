-- ADR-197: a resource runtime change marks the engagements on that resource
-- instead of refusing. The engagement keeps running on the package frozen in
-- its provision effect; this row says which target it may be rebound to.
CREATE TABLE IF NOT EXISTS engagement_runtime_upgrades (
    engagement_id TEXT PRIMARY KEY NOT NULL,
    resource_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('upgrade_pending','pinned','halted')),
    target_runtime TEXT NOT NULL CHECK(json_valid(target_runtime)),
    marked_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS engagement_runtime_upgrades_resource
    ON engagement_runtime_upgrades (resource_id, state);
