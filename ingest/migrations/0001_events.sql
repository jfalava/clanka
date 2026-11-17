-- The diagnostics events installs send to this endpoint (see worker.ts), and
-- who may send them. An event holds numbers, yes or no and short fixed words,
-- never anything said to a companion or remembered by one.

-- One token per person whose events are stored here. They make it with
-- `clanka diagnostics token --new`, which prints its hash, and are let in with
--   INSERT INTO tokens (hash, label, created_at)
--   VALUES ('<hash>', '<label>', unixepoch() * 1000);
CREATE TABLE tokens (
  -- SHA-256 of the token, in hex. The token itself is never stored.
  hash TEXT PRIMARY KEY,
  -- Whose it is. Written on every event the token sends.
  label TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  -- Set to refuse the token from then on. Its events stay.
  revoked_at INTEGER
) STRICT;

CREATE TABLE events (
  id INTEGER PRIMARY KEY,
  received_at INTEGER NOT NULL,
  -- The label of the token that sent it.
  sender TEXT NOT NULL,
  -- The random name the sending machine gave itself.
  install TEXT NOT NULL,
  -- SHA-256 of the line as sent, cut to 32 hex characters.
  digest TEXT NOT NULL,
  -- When it happened on the sending machine, in milliseconds since the epoch.
  at INTEGER NOT NULL,
  event TEXT NOT NULL,
  host TEXT,
  version TEXT,
  os TEXT,
  arch TEXT,
  companion TEXT,
  pass TEXT,
  error TEXT,
  ms INTEGER,
  -- The whole event as sent, for the fields without a column.
  body TEXT NOT NULL CHECK (json_valid(body))
) STRICT;

-- A client sends a line again when the answer to its request was lost. The
-- second copy is dropped here.
CREATE UNIQUE INDEX events_once ON events (install, digest);

CREATE INDEX events_kind ON events (event, at);
