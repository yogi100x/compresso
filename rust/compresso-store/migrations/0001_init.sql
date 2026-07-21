PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE schema_migrations (
  version INTEGER PRIMARY KEY,
  applied_at INTEGER NOT NULL
);

CREATE TABLE repositories (
  id INTEGER PRIMARY KEY,
  root TEXT NOT NULL UNIQUE,            -- canonicalized absolute path
  approved_at INTEGER NOT NULL,
  parser_version TEXT NOT NULL,
  schema_version INTEGER NOT NULL DEFAULT 1,
  last_indexed_at INTEGER
);

CREATE TABLE files (
  id INTEGER PRIMARY KEY,
  repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
  path TEXT NOT NULL,                   -- repo-relative, forward slashes
  hash TEXT NOT NULL,                   -- blake3 hex
  language TEXT,                        -- rust|typescript|tsx|javascript|python|NULL
  size_bytes INTEGER NOT NULL,
  token_estimate INTEGER NOT NULL,
  indexed_at INTEGER NOT NULL,
  UNIQUE (repo_id, path)
);
CREATE INDEX idx_files_repo_lang ON files(repo_id, language);

CREATE TABLE symbols (
  id INTEGER PRIMARY KEY,
  file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  qualified_name TEXT NOT NULL,         -- e.g. SessionManager::start
  kind TEXT NOT NULL,                   -- function|method|struct|class|enum|trait|interface|const|type|module
  signature TEXT,
  start_line INTEGER NOT NULL,          -- 1-based inclusive
  end_line INTEGER NOT NULL,
  token_estimate INTEGER NOT NULL
);
CREATE INDEX idx_symbols_file ON symbols(file_id);
CREATE INDEX idx_symbols_name ON symbols(name);

CREATE TABLE edges (
  id INTEGER PRIMARY KEY,
  repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
  from_file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  to_path TEXT NOT NULL,                -- import target as written, resolved when possible
  to_file_id INTEGER REFERENCES files(id) ON DELETE SET NULL,
  resolution_status TEXT NOT NULL DEFAULT 'unresolved' CHECK (resolution_status IN ('resolved','ambiguous','unresolved')),
  resolution_detail TEXT,
  kind TEXT NOT NULL                    -- imports|references|tests
);
CREATE INDEX idx_edges_from ON edges(from_file_id);
CREATE INDEX idx_edges_to ON edges(to_file_id);

CREATE VIRTUAL TABLE symbols_fts USING fts5(
  name, qualified_name, signature,
  content='symbols', content_rowid='id', tokenize='trigram'
);
CREATE TRIGGER symbols_ai AFTER INSERT ON symbols BEGIN
  INSERT INTO symbols_fts(rowid, name, qualified_name, signature)
  VALUES (new.id, new.name, new.qualified_name, new.signature);
END;
CREATE TRIGGER symbols_ad AFTER DELETE ON symbols BEGIN
  INSERT INTO symbols_fts(symbols_fts, rowid, name, qualified_name, signature)
  VALUES ('delete', old.id, old.name, old.qualified_name, old.signature);
END;

CREATE TABLE sessions (
  id TEXT PRIMARY KEY,                  -- ulid
  client TEXT NOT NULL,                 -- claude-code|codex|cursor|cli|unknown
  transport TEXT NOT NULL,              -- stdio|http
  started_at INTEGER NOT NULL,
  ended_at INTEGER
);

CREATE TABLE operations (
  id TEXT PRIMARY KEY,                  -- ulid = trace.operation_id
  session_id TEXT REFERENCES sessions(id),
  tool TEXT NOT NULL,                   -- compresso_read|compresso_compress|...
  mode TEXT NOT NULL,
  observed_tokens INTEGER NOT NULL,
  applied_tokens INTEGER NOT NULL,
  compression_overhead INTEGER NOT NULL DEFAULT 0,
  fallback_used INTEGER NOT NULL DEFAULT 0,
  tokens_served INTEGER,                -- retrieval tools: tokens returned to the client
  trace_json TEXT,                      -- compression-trace v1, no raw content; NULL for retrieval-only operations
  created_at INTEGER NOT NULL
);
CREATE INDEX idx_operations_session ON operations(session_id, created_at);

CREATE TABLE strategy_results (
  id INTEGER PRIMARY KEY,
  operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
  strategy_id TEXT NOT NULL,
  applied INTEGER NOT NULL,
  before_tokens INTEGER NOT NULL,
  after_tokens INTEGER NOT NULL,
  duration_ms REAL NOT NULL
);

CREATE TABLE memory_facts (
  id TEXT PRIMARY KEY,
  scope TEXT NOT NULL,                  -- global|repo:<root-hash>
  key TEXT NOT NULL,
  content TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE (scope, key)
);

CREATE TABLE integration_receipts (
  id INTEGER PRIMARY KEY,
  client TEXT NOT NULL,                 -- claude-code|codex|cursor
  config_path TEXT NOT NULL,
  keys_owned TEXT NOT NULL,             -- JSON array of exact keys written
  backup_path TEXT NOT NULL,
  installed_at INTEGER NOT NULL,
  compresso_version TEXT NOT NULL,
  uninstalled_at INTEGER
);

CREATE TABLE index_versions (
  repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
  parser_version TEXT NOT NULL,
  schema_version INTEGER NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE update_state (               -- used by updater, Phase 8
  id INTEGER PRIMARY KEY CHECK (id = 1),
  current_version TEXT NOT NULL,
  previous_version TEXT,
  channel TEXT NOT NULL DEFAULT 'stable',
  last_check_at INTEGER
);
