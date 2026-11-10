-- Which exchanges the writer has reflected on, so one whose reflection failed
-- (the model was down, the process exited) is caught up on with a later one
-- instead of being lost. Turns are immutable, so the mark is its own table.
CREATE TABLE reflections (
  companion_turn_id INTEGER PRIMARY KEY REFERENCES turns(id) ON DELETE CASCADE,
  user_turn_id INTEGER NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
  -- 'running' is a claim: no other handle reflects on the exchange meanwhile.
  state TEXT NOT NULL CHECK (state IN ('running', 'done', 'failed')),
  attempts INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
) STRICT;

CREATE INDEX reflections_user_turn ON reflections(user_turn_id);

-- Everything said before this table existed counts as reflected on: whether
-- it was is unknown, and reflecting twice would write the same memories again.
INSERT INTO reflections (companion_turn_id, user_turn_id, state, attempts, updated_at)
SELECT c.id, u.id, 'done', 0, c.created_at
FROM turns c
JOIN turns u ON u.id = (SELECT max(p.id) FROM turns p
                        WHERE p.session_id = c.session_id AND p.id < c.id AND p.role = 'user')
WHERE c.role = 'companion';
