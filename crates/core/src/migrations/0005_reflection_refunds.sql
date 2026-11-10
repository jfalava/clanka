-- Tries that didn't count against an exchange because the model couldn't be
-- reached or memory was frozen. Counted so an outage that never ends, or a
-- model that fails the same way every time, stops being retried.
ALTER TABLE reflections ADD COLUMN refunded INTEGER NOT NULL DEFAULT 0;

-- Every reflection looks for the ones still owed, which are few; without this
-- it read the mark of every exchange ever had.
CREATE INDEX reflections_owed ON reflections(companion_turn_id) WHERE state != 'done';
