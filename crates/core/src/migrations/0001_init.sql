-- One database per companion. Rules that must hold for every process that
-- opens the file live here as constraints and triggers, not only in Rust.

CREATE TABLE meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT;

CREATE TABLE sessions (
  id INTEGER PRIMARY KEY,
  started_at INTEGER NOT NULL,
  ended_at INTEGER,
  -- 'sleep', or 'doctor' when the doctor closed it.
  ended_by TEXT CHECK (ended_by IN ('sleep', 'doctor'))
) STRICT;

-- The immutable transcript: the truth. Only a CLI hard delete removes rows.
CREATE TABLE turns (
  id INTEGER PRIMARY KEY,
  session_id INTEGER NOT NULL REFERENCES sessions(id),
  role TEXT NOT NULL CHECK (role IN ('user', 'companion')),
  content TEXT NOT NULL,
  created_at INTEGER NOT NULL
) STRICT;

CREATE TRIGGER turns_immutable BEFORE UPDATE ON turns
BEGIN
  SELECT RAISE(ABORT, 'turns are immutable');
END;

-- The companion's interpretation of the transcript.
CREATE TABLE memories (
  id INTEGER PRIMARY KEY,
  type TEXT NOT NULL CHECK (type IN (
    'user_fact', 'preference', 'episode', 'relationship',
    'promise', 'opinion', 'open_thread', 'persona_note')),
  level TEXT NOT NULL CHECK (level IN ('faint', 'normal', 'vivid', 'seared')),
  -- JSON object of checkable facts (names, dates, places). Immutable.
  facts TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(facts) AND json_type(facts) = 'object'),
  interpretation TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'superseded')),
  superseded_by INTEGER REFERENCES memories(id),
  occurred_at INTEGER,
  persona_hash TEXT NOT NULL,
  -- Opinions: how strongly the companion holds the stance (0–1).
  strength REAL CHECK (strength IS NULL OR strength BETWEEN 0 AND 1),
  -- Open threads.
  urgency TEXT CHECK (urgency IN ('simmering', 'hot')),
  resolved_at INTEGER,
  recall_count INTEGER NOT NULL DEFAULT 0,
  last_recalled_session INTEGER REFERENCES sessions(id),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  CHECK ((type = 'opinion') = (strength IS NOT NULL)),
  CHECK ((type = 'open_thread') = (urgency IS NOT NULL)),
  CHECK ((status = 'superseded') = (superseded_by IS NOT NULL))
) STRICT;

CREATE INDEX memories_active ON memories(status, type);

CREATE TRIGGER memories_facts_immutable BEFORE UPDATE OF facts ON memories
WHEN NEW.facts IS NOT OLD.facts
BEGIN
  SELECT RAISE(ABORT, 'facts are immutable; supersede the memory instead');
END;

CREATE TRIGGER memories_seared_frozen BEFORE UPDATE OF interpretation, level ON memories
WHEN OLD.level = 'seared'
  AND (NEW.interpretation IS NOT OLD.interpretation OR NEW.level IS NOT OLD.level)
BEGIN
  SELECT RAISE(ABORT, 'seared memories never change');
END;

-- Every change to a memory, including drift, as a new row.
CREATE TABLE memory_versions (
  id INTEGER PRIMARY KEY,
  memory_id INTEGER NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
  interpretation TEXT NOT NULL,
  level TEXT NOT NULL,
  persona_hash TEXT NOT NULL,
  reason TEXT NOT NULL CHECK (reason IN (
    'written', 'drift', 'escalated', 'reinterpreted', 'cli', 'rollback')),
  drift_event_id INTEGER REFERENCES drift_events(id),
  created_at INTEGER NOT NULL
) STRICT;

-- Turns that anchor a memory's facts.
CREATE TABLE memory_sources (
  memory_id INTEGER NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
  turn_id INTEGER NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
  PRIMARY KEY (memory_id, turn_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE drift_events (
  id INTEGER PRIMARY KEY,
  session_id INTEGER NOT NULL REFERENCES sessions(id),
  memory_id INTEGER NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
  -- Stored as text: SQLite integers are signed 64-bit.
  seed TEXT NOT NULL,
  magnitude REAL NOT NULL,
  created_at INTEGER NOT NULL
) STRICT;

-- Editable soul sections (about you, the relationship). The persona is a file.
CREATE TABLE soul_section_versions (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  content TEXT NOT NULL,
  persona_hash TEXT NOT NULL,
  created_at INTEGER NOT NULL
) STRICT;

CREATE INDEX soul_section_versions_name ON soul_section_versions(name, id);

CREATE TABLE mood_states (
  id INTEGER PRIMARY KEY,
  session_id INTEGER REFERENCES sessions(id),
  valence REAL NOT NULL CHECK (valence BETWEEN -1 AND 1),
  energy REAL NOT NULL CHECK (energy BETWEEN -1 AND 1),
  warmth REAL NOT NULL CHECK (warmth BETWEEN -1 AND 1),
  cause TEXT NOT NULL,
  created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE turn_notes (
  id INTEGER PRIMARY KEY,
  turn_id INTEGER NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
  note TEXT NOT NULL,
  created_at INTEGER NOT NULL
) STRICT;

-- Every write, for the doctor's "show writes since" and rollback.
CREATE TABLE write_log (
  id INTEGER PRIMARY KEY,
  actor TEXT NOT NULL CHECK (actor IN ('writer', 'sleep', 'doctor', 'cli')),
  action TEXT NOT NULL,
  target TEXT NOT NULL,
  target_id INTEGER,
  payload TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(payload)),
  created_at INTEGER NOT NULL
) STRICT;

CREATE VIRTUAL TABLE memories_fts
USING fts5(interpretation, facts, content='memories', content_rowid='id');

CREATE TRIGGER memories_fts_ai AFTER INSERT ON memories BEGIN
  INSERT INTO memories_fts(rowid, interpretation, facts)
  VALUES (NEW.id, NEW.interpretation, NEW.facts);
END;

CREATE TRIGGER memories_fts_ad AFTER DELETE ON memories BEGIN
  INSERT INTO memories_fts(memories_fts, rowid, interpretation, facts)
  VALUES ('delete', OLD.id, OLD.interpretation, OLD.facts);
END;

CREATE TRIGGER memories_fts_au AFTER UPDATE OF interpretation, facts ON memories BEGIN
  INSERT INTO memories_fts(memories_fts, rowid, interpretation, facts)
  VALUES ('delete', OLD.id, OLD.interpretation, OLD.facts);
  INSERT INTO memories_fts(rowid, interpretation, facts)
  VALUES (NEW.id, NEW.interpretation, NEW.facts);
END;

-- The vector table is created on open, sized by the embedder (see db.rs).
