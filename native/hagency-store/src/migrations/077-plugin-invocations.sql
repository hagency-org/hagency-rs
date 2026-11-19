CREATE TABLE plugin_invocations (
    invocation_id TEXT PRIMARY KEY,
    plugin_id TEXT NOT NULL,
    package_sha256 TEXT NOT NULL,
    capability TEXT NOT NULL,
    entrypoint_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'succeeded', 'failed', 'outcome_unknown')),
    input_digest TEXT NOT NULL,
    input TEXT NOT NULL,
    result TEXT,
    cleanup TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (plugin_id, package_sha256, capability, entrypoint_id, idempotency_key)
);
CREATE INDEX plugin_invocations_state ON plugin_invocations (plugin_id, state, updated_at);

CREATE TABLE plugin_tool_calls (
    dispatch_id TEXT NOT NULL,
    call_id TEXT NOT NULL,
    plugin_id TEXT NOT NULL,
    tool_id TEXT NOT NULL,
    package_sha256 TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    result TEXT,
    outcome TEXT NOT NULL CHECK (outcome IN ('succeeded', 'failed', 'outcome_unknown')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (dispatch_id, call_id)
);
CREATE INDEX plugin_tool_calls_plugin ON plugin_tool_calls (plugin_id, updated_at);
