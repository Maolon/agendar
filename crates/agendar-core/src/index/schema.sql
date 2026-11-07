-- Agendar local index schema v1. NEVER place this database in the Vault.
-- Normative semantics and transaction procedures: CODE_SPEC.md section 6.
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA busy_timeout = 5000;
PRAGMA user_version = 1;

CREATE TABLE meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT;
-- meta keys: vault_id, schema_version, reducer_version, event_set_digest,
-- generation (decimal), evaluated_at (UTC), last_scan_at (UTC).

CREATE TABLE source_files (
  path TEXT PRIMARY KEY,                    -- Vault-relative, slash separated
  offset INTEGER NOT NULL DEFAULT 0 CHECK(offset >= 0),
  prefix_sha256 TEXT NOT NULL,              -- digest of bytes [0, offset)
  observed_size INTEGER NOT NULL CHECK(observed_size >= 0),
  last_seen_at TEXT NOT NULL,
  health TEXT NOT NULL CHECK(health IN ('ok','partial','missing','rewritten','invalid'))
) STRICT;

CREATE TABLE raw_events (
  id TEXT NOT NULL,
  digest TEXT NOT NULL,                     -- sha256 of canonical JSON
  writer TEXT NOT NULL,
  seq INTEGER NOT NULL CHECK(seq > 0),
  ts TEXT NOT NULL,
  project TEXT NOT NULL,
  kind TEXT NOT NULL,
  json TEXT NOT NULL CHECK(json_valid(json)),
  status TEXT NOT NULL CHECK(status IN ('valid','pending','quarantined')),
  reason TEXT,
  PRIMARY KEY(id, digest)
) STRICT;
-- Deliberately NOT UNIQUE(project,writer,seq): preserve both sides of a fork.
CREATE INDEX raw_writer_seq ON raw_events(project, writer, seq);
CREATE INDEX raw_project_kind ON raw_events(project, kind, ts, id);
CREATE TABLE event_sources (
  path TEXT NOT NULL REFERENCES source_files(path),
  byte_start INTEGER NOT NULL CHECK(byte_start >= 0),
  byte_end INTEGER NOT NULL CHECK(byte_end > byte_start),
  event_id TEXT NOT NULL,
  digest TEXT NOT NULL,
  -- Keep old and new source associations when a provider rewrites a file.
  PRIMARY KEY(path, byte_start, byte_end, event_id, digest),
  FOREIGN KEY(event_id, digest) REFERENCES raw_events(id, digest)
) STRICT;
CREATE TABLE diagnostics (
  id TEXT PRIMARY KEY,
  path TEXT,
  byte_start INTEGER,
  project TEXT,
  event_id TEXT,
  code TEXT NOT NULL,
  message TEXT NOT NULL,
  resolved INTEGER NOT NULL DEFAULT 0 CHECK(resolved IN (0,1))
) STRICT;

-- Fully replace these projection tables in one transaction after each fold.
CREATE TABLE projects (
  slug TEXT PRIMARY KEY,
  archived INTEGER NOT NULL CHECK(archived IN (0,1)),
  variance_rank INTEGER NOT NULL,
  active INTEGER NOT NULL CHECK(active IN (0,1)),
  json TEXT NOT NULL CHECK(json_valid(json))
) STRICT;
CREATE TABLE features (
  project TEXT NOT NULL REFERENCES projects(slug),
  node TEXT NOT NULL,
  progress TEXT NOT NULL CHECK(progress IN ('planned','doing','done','verified','dropped')),
  gate TEXT NOT NULL CHECK(gate IN ('open','awaiting_decision','awaiting_upstream')),
  basis TEXT NOT NULL CHECK(basis IN ('firm','assumed','tainted')),
  liveness TEXT NOT NULL CHECK(liveness IN ('live','quiet','lost')),
  json TEXT NOT NULL CHECK(json_valid(json)),
  PRIMARY KEY(project,node)
) STRICT;
CREATE TABLE edges (
  project TEXT NOT NULL,
  upstream TEXT NOT NULL,
  downstream TEXT NOT NULL,
  source_event TEXT NOT NULL,
  PRIMARY KEY(project,upstream,downstream),
  FOREIGN KEY(project,upstream) REFERENCES features(project,node),
  FOREIGN KEY(project,downstream) REFERENCES features(project,node)
) STRICT;
CREATE TABLE decisions (
  ask_id TEXT PRIMARY KEY,
  project TEXT NOT NULL REFERENCES projects(slug),
  node TEXT NOT NULL,
  lifecycle TEXT NOT NULL CHECK(lifecycle IN ('open','answered','acked','withdrawn','defaulted','amended')),
  effective_answer TEXT,
  json TEXT NOT NULL CHECK(json_valid(json))
) STRICT;
CREATE TABLE receipts (
  id TEXT PRIMARY KEY,
  project TEXT NOT NULL REFERENCES projects(slug),
  ask_id TEXT NOT NULL,
  json TEXT NOT NULL CHECK(json_valid(json))
) STRICT;
-- Observation/review events stay in raw_events; their selected values, histories,
-- causal frontiers and revision tokens are fields of this delivery projection JSON.
-- There are deliberately no separate observations/reviews tables in schema v1.
CREATE TABLE deliveries (
  id TEXT PRIMARY KEY,
  project TEXT NOT NULL REFERENCES projects(slug),
  node TEXT NOT NULL,
  review_status TEXT NOT NULL CHECK(review_status IN ('pending','accepted','sent_back')),
  json TEXT NOT NULL CHECK(json_valid(json))
) STRICT;
CREATE TABLE needs_you (
  id TEXT PRIMARY KEY,
  project TEXT NOT NULL REFERENCES projects(slug),
  kind TEXT NOT NULL CHECK(kind IN ('decide','rescue','confirm','review')),
  score_x2 INTEGER NOT NULL CHECK(score_x2 >= 0),
  since TEXT NOT NULL,
  json TEXT NOT NULL CHECK(json_valid(json))
) STRICT;
CREATE INDEX needs_rank ON needs_you(score_x2 DESC,since,id);
CREATE TABLE rules (
  project TEXT NOT NULL REFERENCES projects(slug),
  rule_id TEXT NOT NULL,
  enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
  json TEXT NOT NULL CHECK(json_valid(json)),
  PRIMARY KEY(project,rule_id)
) STRICT;
CREATE TABLE sessions (
  session_id TEXT PRIMARY KEY,
  project TEXT NOT NULL REFERENCES projects(slug),
  host TEXT NOT NULL,
  harness TEXT NOT NULL,
  last_event_ts TEXT NOT NULL,
  liveness TEXT NOT NULL CHECK(liveness IN ('live','quiet','lost')),
  json TEXT NOT NULL CHECK(json_valid(json))
) STRICT;

-- Local observations: exclude from reducer/convergence digest and preserve on rebuild.
CREATE TABLE local_sessions (
  session_id TEXT PRIMARY KEY,
  project TEXT NOT NULL,
  harness TEXT NOT NULL,
  harness_session_id TEXT NOT NULL,
  pid INTEGER NOT NULL CHECK(pid > 0),
  process_start INTEGER NOT NULL CHECK(process_start >= 0),
  registered_at TEXT NOT NULL,
  repo_root TEXT NOT NULL,
  stopped_at TEXT,
  wait_ask_id TEXT,
  wait_lease_until TEXT
) STRICT;
CREATE TABLE delivery_attempts (
  packet_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('pending','accepted','failed')),
  attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
  next_attempt_at TEXT,
  last_error_code TEXT,
  PRIMARY KEY(packet_id,session_id)
) STRICT;
CREATE TABLE ui_local (
  key TEXT PRIMARY KEY,
  json TEXT NOT NULL CHECK(json_valid(json))
) STRICT;
-- ui_local stores last-look watermark, per-item snooze, notification dedupe,
-- selected theme, motion toggle, and explicit per-host check approvals.
