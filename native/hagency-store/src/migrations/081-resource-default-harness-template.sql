-- ADR-198: one operator-set default harness template per resource. An agent
-- request that matches no enabled agent definition resolves its frozen
-- template from this row; without it the request is refused up front.
CREATE TABLE IF NOT EXISTS resource_default_harness_templates (
    resource_id TEXT PRIMARY KEY NOT NULL,
    template TEXT NOT NULL,
    template_sha256 TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
