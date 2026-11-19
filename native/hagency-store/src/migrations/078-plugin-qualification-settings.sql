ALTER TABLE plugin_runtime_qualifications RENAME TO plugin_runtime_qualifications_old;

CREATE TABLE plugin_runtime_qualifications (
    plugin_id TEXT NOT NULL,
    adapter_id TEXT NOT NULL,
    package_sha256 TEXT NOT NULL,
    settings_sha256 TEXT NOT NULL,
    framework TEXT NOT NULL,
    model TEXT NOT NULL,
    provider TEXT NOT NULL,
    reasoning TEXT,
    roles TEXT NOT NULL,
    evidence_digest TEXT NOT NULL,
    qualified_at INTEGER NOT NULL,
    evidence TEXT NOT NULL,
    PRIMARY KEY (plugin_id, adapter_id, package_sha256, settings_sha256, framework, model, provider, reasoning)
);

INSERT INTO plugin_runtime_qualifications (
    plugin_id, adapter_id, package_sha256, settings_sha256, framework, model,
    provider, reasoning, roles, evidence_digest, qualified_at, evidence
)
SELECT plugin_id, adapter_id, package_sha256, '0000000000000000000000000000000000000000000000000000000000000000', framework, model,
       provider, reasoning, roles, evidence_digest, qualified_at, evidence
FROM plugin_runtime_qualifications_old;

DROP TABLE plugin_runtime_qualifications_old;
UPDATE resources
SET config = json_set(config, '$.runtimePlugin.settingsSha256', '0000000000000000000000000000000000000000000000000000000000000000')
WHERE json_type(config, '$.runtimePlugin') = 'object'
  AND json_type(config, '$.runtimePlugin.settingsSha256') IS NULL;
UPDATE resources
SET config = json_set(config, '$.harnessPlugin.settingsSha256', '0000000000000000000000000000000000000000000000000000000000000000')
WHERE json_type(config, '$.harnessPlugin') = 'object'
  AND json_type(config, '$.harnessPlugin.settingsSha256') IS NULL;

CREATE INDEX plugin_runtime_qualifications_lookup
    ON plugin_runtime_qualifications (framework, model, provider, reasoning);
