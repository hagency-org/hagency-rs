-- ADR-197: guarded per-engagement rebind to a new runtime package.
-- One-step rollback slot: the runtime an engagement was bound to before its
-- last rebind. Its package stays referenced (never collected) while it lives.
CREATE TABLE IF NOT EXISTS engagement_runtime_rollbacks (
    engagement_id TEXT PRIMARY KEY NOT NULL,
    previous_runtime TEXT NOT NULL CHECK(json_valid(previous_runtime)),
    rebound_at INTEGER NOT NULL
);
-- Rolling rollout per resource. The first engagement rebound is the canary.
CREATE TABLE IF NOT EXISTS engagement_runtime_rollouts (
    resource_id TEXT PRIMARY KEY NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('running','halted','complete')),
    target_runtime TEXT NOT NULL CHECK(json_valid(target_runtime)),
    canary_engagement_id TEXT,
    canary_result TEXT CHECK(canary_result IS NULL OR canary_result IN ('passed','failed')),
    canary_rebound_at INTEGER,
    detail TEXT,
    started_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
-- Append-only audit of every rebind, rollback, pin, unpin, refusal and halt.
CREATE TABLE IF NOT EXISTS engagement_runtime_rebinds (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    engagement_id TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    action TEXT NOT NULL CHECK(action IN ('rebind','rollback','pin','unpin','rollout')),
    old_package_sha256 TEXT,
    new_package_sha256 TEXT,
    old_settings_sha256 TEXT,
    new_settings_sha256 TEXT,
    actor TEXT NOT NULL,
    at INTEGER NOT NULL,
    outcome TEXT NOT NULL,
    detail TEXT
);
CREATE INDEX IF NOT EXISTS engagement_runtime_rebinds_engagement
    ON engagement_runtime_rebinds (engagement_id, id);
CREATE TRIGGER IF NOT EXISTS engagement_runtime_rebinds_no_update
BEFORE UPDATE ON engagement_runtime_rebinds
BEGIN SELECT RAISE(ABORT,'engagement runtime audit is append-only'); END;
CREATE TRIGGER IF NOT EXISTS engagement_runtime_rebinds_no_delete
BEFORE DELETE ON engagement_runtime_rebinds
BEGIN SELECT RAISE(ABORT,'engagement runtime audit is append-only'); END;
