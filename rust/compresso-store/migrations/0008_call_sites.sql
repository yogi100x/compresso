-- Call sites extracted by the Atlas parsers (D89). One row per distinct call
-- form inside an indexed function or method. The callee is stored as written
-- (name, optional qualifier, call form); it is resolved to an indexed symbol at
-- query time against the current symbols and import edges, so a row never
-- holds a symbol id that a later re-index of another file could invalidate.
CREATE TABLE call_sites (
  id INTEGER PRIMARY KEY,
  repo_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
  from_file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
  caller_symbol_id INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
  callee_name TEXT NOT NULL,
  callee_qualifier TEXT,
  call_form TEXT NOT NULL CHECK (call_form IN ('plain','path','self','receiver')),
  line INTEGER NOT NULL CHECK (line >= 1)
);
CREATE INDEX idx_call_sites_caller ON call_sites(caller_symbol_id);
CREATE INDEX idx_call_sites_callee ON call_sites(repo_id, callee_name);
CREATE INDEX idx_call_sites_file ON call_sites(from_file_id);
