-- Each time she pushed back on him, for the rolling pushback budget. Clashes
-- with a core value are recorded too but never count against the budget.
CREATE TABLE pushbacks (
  id INTEGER PRIMARY KEY,
  turn_id INTEGER NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
  core_value INTEGER NOT NULL CHECK (core_value IN (0, 1)),
  about TEXT NOT NULL,
  created_at INTEGER NOT NULL
) STRICT;

CREATE INDEX pushbacks_turn ON pushbacks(turn_id);
