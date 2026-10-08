-- Wave 7: Terminal Health (docs/OPERATIONAL_CONTROL.md).
--
-- `device_heartbeats` stays the hub's one record of what each terminal
-- reported. New fields stay NULL until a terminal that knows them reports
-- them: nothing is filled in for terminals that have not said so.
ALTER TABLE device_heartbeats ADD COLUMN protocol_version INTEGER;
-- When the oldest change still waiting to be sent was made.
ALTER TABLE device_heartbeats ADD COLUMN oldest_pending_at TEXT;
-- When the terminal last completed a full exchange with the hub.
ALTER TABLE device_heartbeats ADD COLUMN last_sync_ok_at TEXT;
-- How many records the terminal still holds as refused.
ALTER TABLE device_heartbeats ADD COLUMN problem_count INTEGER;
-- When the last heartbeat (not just any request) arrived.
ALTER TABLE device_heartbeats ADD COLUMN last_heartbeat_at TEXT;
