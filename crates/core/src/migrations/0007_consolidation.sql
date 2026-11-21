-- A night ends when the goodnight reply is recorded, and sleep (or waking,
-- for a dream) consolidates it behind, so nobody waits for the model. A
-- session ended by 'sleep' with no consolidated_at still waits for that pass.
ALTER TABLE sessions ADD COLUMN consolidated_at INTEGER;

-- Passes over it that failed on the model's side; past a few it is left.
ALTER TABLE sessions ADD COLUMN consolidation_attempts INTEGER NOT NULL DEFAULT 0;

-- Every night slept before this was consolidated as it ended.
UPDATE sessions SET consolidated_at = ended_at WHERE ended_by = 'sleep';
