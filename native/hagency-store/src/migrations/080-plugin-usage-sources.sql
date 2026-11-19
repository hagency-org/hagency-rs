-- Plugin dispatches bind an explicitly unmetered usage source.
-- SQLite cannot widen the existing framework CHECK in place, so rebuild both
-- usage tables while preserving every historical row and foreign-key edge.
CREATE TEMP TABLE usage_sources_080 AS SELECT * FROM usage_sources;
CREATE TEMP TABLE usage_receipts_080 AS SELECT * FROM usage_receipts;
DROP TABLE usage_receipts;
DROP TABLE usage_sources;
CREATE TABLE usage_sources (
 id TEXT PRIMARY KEY, dispatch_id TEXT NOT NULL REFERENCES runner_dispatches(id),
 fence INTEGER NOT NULL, engagement_id TEXT NOT NULL REFERENCES engagements(id),
 identity_digest TEXT NOT NULL, framework TEXT NOT NULL CHECK(length(framework) BETWEEN 1 AND 64),
 attribution TEXT NOT NULL CHECK(json_valid(attribution)),
 high_water TEXT NOT NULL CHECK(json_valid(high_water)),
 latest_counts TEXT NOT NULL CHECK(json_valid(latest_counts)),
 latest_observation TEXT CHECK(latest_observation IS NULL OR json_valid(latest_observation)),
 latest_incomplete INTEGER NOT NULL DEFAULT 1 CHECK(latest_incomplete IN (0,1)),
 latest_regressed INTEGER NOT NULL DEFAULT 0 CHECK(latest_regressed IN (0,1)),
 historical_incomplete INTEGER NOT NULL DEFAULT 0 CHECK(historical_incomplete IN (0,1)),
 regressions INTEGER NOT NULL DEFAULT 0, observations INTEGER NOT NULL DEFAULT 0,
 observed_at INTEGER, UNIQUE(dispatch_id,fence)
) STRICT;
CREATE INDEX usage_sources_engagement ON usage_sources(engagement_id);
CREATE TABLE usage_receipts (
 source_id TEXT NOT NULL REFERENCES usage_sources(id), call_id TEXT NOT NULL,
 digest TEXT NOT NULL, observation TEXT NOT NULL CHECK(json_valid(observation)),
 response TEXT NOT NULL CHECK(json_valid(response)), PRIMARY KEY(source_id,call_id)
) STRICT;
INSERT INTO usage_sources(id,dispatch_id,fence,engagement_id,identity_digest,framework,attribution,high_water,latest_counts,latest_observation,latest_incomplete,latest_regressed,historical_incomplete,regressions,observations,observed_at)
 SELECT id,dispatch_id,fence,engagement_id,identity_digest,framework,attribution,high_water,latest_counts,latest_observation,latest_incomplete,latest_regressed,historical_incomplete,regressions,observations,observed_at FROM temp.usage_sources_080;
INSERT INTO usage_receipts(source_id,call_id,digest,observation,response)
 SELECT source_id,call_id,digest,observation,response FROM temp.usage_receipts_080;
DROP TABLE temp.usage_sources_080;
DROP TABLE temp.usage_receipts_080;