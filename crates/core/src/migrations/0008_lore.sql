-- Lore: shared world reference that lives in files outside the database
-- (crate::lore). What is the companion's own about it lives here.

-- What the companion knows about an entry, private unless `released_line`
-- names the ledger line that published it. Deleted only by
-- `clanka lore forget`.
CREATE TABLE lore_notes (
  id INTEGER PRIMARY KEY,
  entry TEXT NOT NULL,
  text TEXT NOT NULL,
  provenance TEXT NOT NULL CHECK (provenance IN ('told', 'worked_out', 'dreamt')),
  session_id INTEGER REFERENCES sessions(id),
  released_line TEXT,
  created_at INTEGER NOT NULL
) STRICT;

CREATE INDEX lore_notes_entry ON lore_notes(entry, id);

-- Entries open in a session, read most recently last. `shown` is the hash of
-- the entry as last shown to the model, so a host that replays its history
-- isn't sent the same text again.
CREATE TABLE lore_open (
  session_id INTEGER NOT NULL REFERENCES sessions(id),
  entry TEXT NOT NULL,
  read_at INTEGER NOT NULL,
  shown TEXT,
  PRIMARY KEY (session_id, entry)
) STRICT;

-- Read receipts: the hash of the entry's text and the newest ledger line as
-- last read.
CREATE TABLE lore_reads (
  entry TEXT PRIMARY KEY,
  text_hash TEXT NOT NULL,
  last_line TEXT,
  read_at INTEGER NOT NULL
) STRICT;

-- Entries the companion may read without a grant in its soul: one an entry
-- it read `opens`, or a stub it made.
CREATE TABLE lore_earned (
  entry TEXT PRIMARY KEY,
  via TEXT NOT NULL,
  earned_at INTEGER NOT NULL
) STRICT;

-- Ledger lines the companion wrote, kept until they are appended to the
-- entry's ledger file, so a lock timeout or a crash only delays them.
CREATE TABLE lore_outbox (
  id INTEGER PRIMARY KEY,
  entry TEXT NOT NULL,
  kind TEXT NOT NULL,
  line TEXT NOT NULL CHECK (json_valid(line)),
  session_id INTEGER REFERENCES sessions(id),
  created_at INTEGER NOT NULL,
  flushed_at INTEGER
) STRICT;

CREATE INDEX lore_outbox_unflushed ON lore_outbox(flushed_at, id);
CREATE INDEX lore_outbox_session ON lore_outbox(session_id, kind);
