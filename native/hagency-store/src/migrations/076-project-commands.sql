-- Business results are independent of transport acknowledgements and display
-- history. Keep them through retries/restarts; never replay a deleted receipt.
CREATE TABLE project_command_receipts (
    fleet_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    command_id TEXT NOT NULL,
    digest TEXT NOT NULL,
    receipt TEXT NOT NULL CHECK(json_valid(receipt)),
    published INTEGER NOT NULL DEFAULT 0 CHECK(published IN (0,1)),
    PRIMARY KEY(fleet_id,generation,command_id)
) STRICT;
CREATE INDEX project_command_receipts_pending ON project_command_receipts(fleet_id,generation,published);
CREATE INDEX project_command_receipts_agent ON project_command_receipts(fleet_id,generation,json_extract(receipt,'$.outcome.result.engagementId'));
