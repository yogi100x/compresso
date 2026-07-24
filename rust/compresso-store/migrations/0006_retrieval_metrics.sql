ALTER TABLE operations ADD COLUMN repository_id INTEGER REFERENCES repositories(id) ON DELETE SET NULL;
ALTER TABLE operations ADD COLUMN estimated_raw_source_tokens INTEGER;
ALTER TABLE operations ADD COLUMN modeled_follow_up_calls_avoided INTEGER;

CREATE INDEX idx_operations_repository
  ON operations(repository_id, created_at);
