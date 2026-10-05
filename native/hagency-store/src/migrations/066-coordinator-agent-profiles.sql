-- A friendly Matrix label never changes an allocation, runtime or resource name.
CREATE TABLE coordinator_agent_profiles (
    agent_id TEXT PRIMARY KEY REFERENCES coordinator_agents(agent_id),
    command_id TEXT NOT NULL,
    desired_name TEXT NOT NULL,
    confirmed_name TEXT,
    last_error TEXT,
    updated_at INTEGER NOT NULL,
    observed_at INTEGER
) STRICT;
