-- Rinx ADR 0010: provider-owned capacity -> projects -> agents.
-- Revocation/expiry stops admission but does not silently release reservations.
CREATE TABLE resource_delegations (
    id TEXT PRIMARY KEY,
    fleet_id TEXT NOT NULL REFERENCES registrations(fleet_id),
    resource_id TEXT NOT NULL REFERENCES resources(id),
    config TEXT NOT NULL CHECK(json_valid(config)),
    revoked_at INTEGER,
    created_at INTEGER NOT NULL
) STRICT;
CREATE INDEX resource_delegations_resource ON resource_delegations(resource_id);
CREATE TABLE project_grants (
    id TEXT PRIMARY KEY,
    delegation_id TEXT NOT NULL REFERENCES resource_delegations(id),
    fleet_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    config TEXT NOT NULL CHECK(json_valid(config)),
    revoked_at INTEGER,
    created_at INTEGER NOT NULL,
    UNIQUE(fleet_id,project_id,resource_id)
) STRICT;
CREATE INDEX project_grants_parent ON project_grants(delegation_id);
CREATE TABLE project_grant_agents (
    -- Keep budget debits after engagement retention; deletion is not a refund.
    engagement_id TEXT PRIMARY KEY,
    grant_id TEXT NOT NULL REFERENCES project_grants(id),
    debited_tokens INTEGER NOT NULL CHECK(debited_tokens > 0),
    decision_actor TEXT NOT NULL,
    grant_revision INTEGER NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;
CREATE INDEX project_grant_agents_grant ON project_grant_agents(grant_id);

-- Financial decisions must outlive the rotating display/audit history. Never
-- drop these receipts merely because ordinary engagement decisions are pruned.
CREATE TABLE project_grant_decisions (
    id TEXT PRIMARY KEY,
    digest TEXT NOT NULL,
    result TEXT NOT NULL CHECK(json_valid(result)),
    created_at INTEGER NOT NULL
) STRICT;
