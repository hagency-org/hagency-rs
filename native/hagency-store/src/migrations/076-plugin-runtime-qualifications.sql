CREATE TABLE plugin_runtime_qualifications (
    plugin_id TEXT NOT NULL,
    adapter_id TEXT NOT NULL,
    package_sha256 TEXT NOT NULL,
    framework TEXT NOT NULL,
    model TEXT NOT NULL,
    provider TEXT NOT NULL,
    reasoning TEXT,
    roles TEXT NOT NULL,
    evidence_digest TEXT NOT NULL,
    qualified_at INTEGER NOT NULL,
    evidence TEXT NOT NULL,
    PRIMARY KEY (plugin_id, adapter_id, package_sha256, framework, model, provider, reasoning)
);
CREATE INDEX plugin_runtime_qualifications_lookup
    ON plugin_runtime_qualifications (framework, model, provider, reasoning);
