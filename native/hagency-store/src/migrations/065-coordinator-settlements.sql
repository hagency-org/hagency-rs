-- A terminal runtime state alone never releases unknown consumption.
CREATE TABLE coordinator_settlements (
    agent_id TEXT PRIMARY KEY REFERENCES coordinator_agents(agent_id),
    command_id TEXT NOT NULL UNIQUE,
    digest TEXT NOT NULL,
    receipt TEXT NOT NULL CHECK(json_valid(receipt)),
    accepted_at INTEGER NOT NULL
) STRICT;
