-- Dreams: sessions the user narrates and the companion lives through, then
-- wakes from. Columns are added rather than CHECKs widened: SQLite can't
-- change a CHECK without rebuilding the table.

-- A dream. One that woke ended by 'sleep' (the wake pass); one its front end
-- lost was dropped, and has an ended_at but no ended_by.
ALTER TABLE sessions ADD COLUMN dream INTEGER NOT NULL DEFAULT 0 CHECK (dream IN (0, 1));

-- A dream message marked as an echo: a scene from the past. A fact survives a
-- dream only if an echo showed it.
ALTER TABLE turns ADD COLUMN echo INTEGER NOT NULL DEFAULT 0 CHECK (echo IN (0, 1));

-- Made in a dream: a fragment (faint, no facts, recalled only in later dreams)
-- or the one thing an echo seared in, recalled awake as learned in a dream.
ALTER TABLE memories ADD COLUMN dreamt INTEGER NOT NULL DEFAULT 0
  CHECK (dreamt IN (0, 1) AND (dreamt = 0 OR level = 'seared' OR (level = 'faint' AND facts = '{}')));
