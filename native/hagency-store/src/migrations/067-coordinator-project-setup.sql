CREATE TABLE coordinator_project_setup_attempts (
    id TEXT PRIMARY KEY,
    engagement_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    digest TEXT NOT NULL,
    command TEXT NOT NULL CHECK(json_valid(command)),
    result TEXT CHECK(result IS NULL OR json_valid(result)),
    FOREIGN KEY(engagement_id,project_id) REFERENCES coordinator_projects(engagement_id,id)
) STRICT;
CREATE TABLE coordinator_project_setup (
    engagement_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL REFERENCES coordinator_project_setup_attempts(id),
    observation TEXT NOT NULL CHECK(json_valid(observation)),
    PRIMARY KEY(engagement_id,project_id),
    FOREIGN KEY(engagement_id,project_id) REFERENCES coordinator_projects(engagement_id,id)
) STRICT;
