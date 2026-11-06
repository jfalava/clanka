-- How the memory feels to the companion, −1 to 1. Drives mood-congruent recall: a sad
-- mood surfaces sad memories first. NULL means neutral or unknown.
ALTER TABLE memories ADD COLUMN valence REAL
  CHECK (valence IS NULL OR valence BETWEEN -1 AND 1);
