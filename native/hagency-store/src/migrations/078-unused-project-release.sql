-- Retain the grant as a permanent admission/replay fence. Only the explicit
-- no-lifetime-debits release transaction may set this marker.
ALTER TABLE project_grants ADD COLUMN released_at INTEGER;
