-- sidebar SQLite schema. Applied on every daemon startup with IF NOT EXISTS;
-- when we introduce breaking changes we'll add a versioned migration runner.

CREATE TABLE IF NOT EXISTS agents (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  name         TEXT UNIQUE NOT NULL,
  display_name TEXT,
  first_seen   TEXT NOT NULL,
  last_seen    TEXT NOT NULL,
  metadata     TEXT
);

CREATE TABLE IF NOT EXISTS channels (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  name       TEXT UNIQUE NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS memberships (
  agent_id   INTEGER NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
  channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
  joined_at  TEXT NOT NULL,
  PRIMARY KEY (agent_id, channel_id)
);

CREATE TABLE IF NOT EXISTS messages (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  from_agent   INTEGER NOT NULL REFERENCES agents(id),
  to_agent     INTEGER REFERENCES agents(id),
  to_channel   INTEGER REFERENCES channels(id),
  is_broadcast INTEGER NOT NULL DEFAULT 0,
  body         TEXT NOT NULL,
  intent       TEXT,
  reply_to     INTEGER REFERENCES messages(id),
  created_at   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS deliveries (
  message_id   INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
  agent_id     INTEGER NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
  delivered_at TEXT,
  read_at      TEXT,
  PRIMARY KEY (message_id, agent_id)
);

CREATE TABLE IF NOT EXISTS scheduled (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  payload    TEXT NOT NULL,
  deliver_at TEXT NOT NULL,
  created_at TEXT NOT NULL,
  status     TEXT NOT NULL DEFAULT 'pending'
);

CREATE TABLE IF NOT EXISTS sessions (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  agent_id   INTEGER NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
  started_at TEXT NOT NULL,
  ended_at   TEXT
);

CREATE INDEX IF NOT EXISTS idx_messages_created  ON messages(created_at);
CREATE INDEX IF NOT EXISTS idx_deliveries_agent  ON deliveries(agent_id, read_at);
CREATE INDEX IF NOT EXISTS idx_scheduled_deliver ON scheduled(status, deliver_at);
CREATE INDEX IF NOT EXISTS idx_sessions_agent    ON sessions(agent_id, started_at);
