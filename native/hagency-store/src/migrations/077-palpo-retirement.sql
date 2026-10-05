-- Remote Matrix retirement is independent of the local retirement effect.
-- Keep the exact verified identity through publication retries and restarts.
CREATE TABLE palpo_agent_retirements (
    engagement_id TEXT PRIMARY KEY REFERENCES engagements(id) ON DELETE CASCADE,
    generation INTEGER NOT NULL,
    target TEXT NOT NULL CHECK(json_valid(target)),
    confirmed_at INTEGER NOT NULL
) STRICT;
