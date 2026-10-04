-- Server engagements and child resource grants are separate from the retained
-- `engagements` table, whose rows continue to mean individual agent allocations.
CREATE TABLE coordinator_engagements (
    id TEXT PRIMARY KEY REFERENCES registrations(fleet_id),
    authority TEXT NOT NULL CHECK(json_valid(authority))
) STRICT;
CREATE TABLE coordinator_resources (
    id TEXT PRIMARY KEY,
    engagement_id TEXT NOT NULL REFERENCES coordinator_engagements(id),
    resource_id TEXT NOT NULL REFERENCES resources(id),
    preset_id TEXT NOT NULL,
    seat_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    allocated_tokens INTEGER NOT NULL CHECK(allocated_tokens >= 0),
    period TEXT NOT NULL,
    period_key TEXT NOT NULL,
    managers TEXT NOT NULL CHECK(json_valid(managers)),
    UNIQUE(engagement_id,resource_id,period,period_key)
) STRICT;
CREATE TABLE coordinator_projects (
    id TEXT NOT NULL,
    engagement_id TEXT NOT NULL REFERENCES coordinator_engagements(id),
    grant TEXT NOT NULL CHECK(json_valid(grant)),
    definition TEXT NOT NULL CHECK(json_valid(definition)),
    PRIMARY KEY(engagement_id,id)
) STRICT;
CREATE TABLE coordinator_agents (
    agent_id TEXT PRIMARY KEY REFERENCES engagements(id),
    resource_allocation_id TEXT NOT NULL REFERENCES coordinator_resources(id),
    allocated_tokens INTEGER NOT NULL CHECK(allocated_tokens > 0),
    -- Retirement does not erase spent or unknown tokens. Explicit settlement is
    -- required before reducing this retained reservation.
    retained_tokens INTEGER NOT NULL CHECK(retained_tokens >= 0),
    command_id TEXT NOT NULL UNIQUE,
    decision TEXT NOT NULL CHECK(json_valid(decision))
) STRICT;
CREATE TABLE coordinator_commands (
    id TEXT PRIMARY KEY,
    engagement_id TEXT NOT NULL REFERENCES coordinator_engagements(id),
    digest TEXT NOT NULL,
    receipt TEXT NOT NULL CHECK(json_valid(receipt))
) STRICT;
CREATE INDEX coordinator_resources_parent ON coordinator_resources(resource_id);
CREATE INDEX coordinator_agents_resource ON coordinator_agents(resource_allocation_id);
-- Latest projection per entity. Acknowledging an older frozen publication must
-- never erase a newer revision queued while the network request was in flight.
CREATE TABLE coordinator_publications (
    engagement_id TEXT NOT NULL REFERENCES coordinator_engagements(id),
    id TEXT NOT NULL,
    digest TEXT NOT NULL,
    payload TEXT NOT NULL CHECK(json_valid(payload)),
    PRIMARY KEY(engagement_id,id)
) STRICT;
