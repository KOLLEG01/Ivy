export const STORAGE_FORMAT = 4;
export const schemaSql = `
CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
CREATE TABLE principals (id TEXT PRIMARY KEY) STRICT;
CREATE TABLE credentials (
  digest TEXT PRIMARY KEY, principal_id TEXT NOT NULL REFERENCES principals(id), revoked INTEGER NOT NULL CHECK(revoked IN (0,1))
) STRICT;
CREATE TABLE browser_sessions (
  digest TEXT PRIMARY KEY, credential_digest TEXT NOT NULL REFERENCES credentials(digest), created_at TEXT NOT NULL
) STRICT;
CREATE TABLE contract_families (
  key TEXT PRIMARY KEY, owner TEXT NOT NULL, media_type TEXT NOT NULL,
  object_mode TEXT NOT NULL CHECK(object_mode IN ('retain','owned'))
) STRICT;
CREATE TABLE contracts (
  key TEXT NOT NULL REFERENCES contract_families(key), version TEXT NOT NULL, definition TEXT NOT NULL,
  PRIMARY KEY(key, version)
) STRICT;
CREATE TABLE objects (
  search_id INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  parent_id TEXT REFERENCES objects(id), owner_object_id TEXT REFERENCES objects(id), name TEXT NOT NULL, path TEXT NOT NULL UNIQUE,
  position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 9007199254740991),
  icon TEXT CHECK(icon IS NULL OR length(icon) BETWEEN 1 AND 32),
  depth INTEGER NOT NULL CHECK(depth BETWEEN 1 AND 64), contract_key TEXT NOT NULL REFERENCES contract_families(key),
  current_revision INTEGER NOT NULL CHECK(current_revision BETWEEN 1 AND 9007199254740991),
  archived_at TEXT, effective_archive INTEGER NOT NULL CHECK(effective_archive IN (0,1)),
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  FOREIGN KEY(id, current_revision) REFERENCES revisions(object_id, revision) DEFERRABLE INITIALLY DEFERRED
) STRICT;
CREATE UNIQUE INDEX objects_root_name ON objects(name COLLATE BINARY) WHERE parent_id IS NULL;
CREATE UNIQUE INDEX objects_parent_name ON objects(parent_id, name COLLATE BINARY) WHERE parent_id IS NOT NULL;
CREATE INDEX objects_root_position ON objects(position, id) WHERE parent_id IS NULL;
CREATE INDEX objects_parent_position ON objects(parent_id, position, id) WHERE parent_id IS NOT NULL;
CREATE INDEX objects_owner ON objects(owner_object_id) WHERE owner_object_id IS NOT NULL;
CREATE INDEX objects_contract ON objects(contract_key, effective_archive, id);
CREATE TABLE revisions (
  object_id TEXT NOT NULL REFERENCES objects(id), revision INTEGER NOT NULL CHECK(revision BETWEEN 1 AND 9007199254740991),
  contract_key TEXT NOT NULL, contract_version TEXT NOT NULL, encoding TEXT NOT NULL CHECK(encoding IN ('json','text','base64')),
  content ANY NOT NULL, content_hash TEXT NOT NULL, byte_length INTEGER NOT NULL CHECK(byte_length BETWEEN 0 AND 8388608),
  created_at TEXT NOT NULL, references_json TEXT NOT NULL CHECK(json_valid(references_json)), PRIMARY KEY(object_id, revision),
  FOREIGN KEY(contract_key, contract_version) REFERENCES contracts(key, version),
  CHECK((encoding = 'base64' AND typeof(content) = 'blob') OR (encoding != 'base64' AND typeof(content) = 'text'))
) STRICT;
CREATE INDEX revisions_contract_version ON revisions(contract_key,contract_version);
CREATE TABLE revision_references (
  source_object_id TEXT NOT NULL, source_revision INTEGER NOT NULL, name TEXT NOT NULL,
  target_object_id TEXT NOT NULL, target_revision INTEGER NOT NULL,
  PRIMARY KEY(source_object_id, source_revision, name),
  FOREIGN KEY(source_object_id, source_revision) REFERENCES revisions(object_id, revision) ON DELETE CASCADE,
  FOREIGN KEY(target_object_id, target_revision) REFERENCES revisions(object_id, revision) ON DELETE RESTRICT
) STRICT;
CREATE INDEX revision_references_target ON revision_references(target_object_id,target_revision);
CREATE VIRTUAL TABLE object_fts USING fts5(object_id UNINDEXED, name, text, tokenize='unicode61');
CREATE TABLE mutations (
  principal_id TEXT NOT NULL REFERENCES principals(id), mutation_id TEXT NOT NULL, request_hash TEXT NOT NULL,
  issued_at_ms INTEGER NOT NULL, completed_at_ms INTEGER NOT NULL,
  result_kind TEXT NOT NULL, result_json TEXT NOT NULL CHECK(length(result_json) <= 65536),
  receipt_bytes INTEGER NOT NULL CHECK(receipt_bytes >= 0),
  PRIMARY KEY(principal_id, mutation_id)
) STRICT;
CREATE TABLE topics (
  topic TEXT NOT NULL, version TEXT NOT NULL, owner TEXT NOT NULL, definition TEXT NOT NULL,
  PRIMARY KEY(topic, version)
) STRICT;
CREATE TABLE events (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT CHECK(sequence BETWEEN 1 AND 9007199254740991),
  topic TEXT NOT NULL, topic_version TEXT NOT NULL, source TEXT NOT NULL, occurred_at TEXT NOT NULL,
  mutation_id TEXT NOT NULL, payload TEXT NOT NULL,
  FOREIGN KEY(topic, topic_version) REFERENCES topics(topic, version)
) STRICT;
CREATE INDEX events_topic_sequence ON events(topic, sequence);
CREATE TABLE retention_families (
  contract_key TEXT PRIMARY KEY REFERENCES contract_families(key), policy_hash TEXT NOT NULL,
  previewed INTEGER NOT NULL CHECK(previewed IN (0,1)), summary_json TEXT NOT NULL
) STRICT;
CREATE TABLE service_nodes (
  id TEXT PRIMARY KEY, principal_id TEXT NOT NULL REFERENCES principals(id), host_id TEXT NOT NULL, service_name TEXT NOT NULL,
  generation INTEGER NOT NULL CHECK(generation BETWEEN 1 AND 9007199254740991), status_json TEXT NOT NULL
) STRICT;
CREATE INDEX service_nodes_host ON service_nodes(host_id, service_name);
CREATE TABLE host_reports (
  host_id TEXT PRIMARY KEY, node_id TEXT NOT NULL REFERENCES service_nodes(id), sequence INTEGER NOT NULL,
  reported_at TEXT NOT NULL, snapshot_json TEXT NOT NULL
) STRICT;
CREATE TABLE managed_nodes (
  node_id TEXT PRIMARY KEY, host_id TEXT NOT NULL, desired_enabled INTEGER NOT NULL CHECK(desired_enabled IN (0,1))
) STRICT;
CREATE TABLE service_connections (
  node_id TEXT PRIMARY KEY REFERENCES service_nodes(id), credential_digest TEXT NOT NULL REFERENCES credentials(digest), session_digest TEXT
) STRICT;
CREATE TABLE subscriptions (
  node_id TEXT NOT NULL REFERENCES service_nodes(id), name TEXT NOT NULL, filter_json TEXT NOT NULL,
  initial_sequence INTEGER NOT NULL, acknowledged_sequence INTEGER NOT NULL,
  batch_generation INTEGER, batch_json TEXT, PRIMARY KEY(node_id, name)
) STRICT;
CREATE TABLE durable_subscriptions (
  node_id TEXT NOT NULL, name TEXT NOT NULL, PRIMARY KEY(node_id, name),
  FOREIGN KEY(node_id, name) REFERENCES subscriptions(node_id, name)
) STRICT;
CREATE TABLE registries (node_id TEXT PRIMARY KEY REFERENCES service_nodes(id), registry_json TEXT NOT NULL) STRICT;
CREATE TABLE tool_definitions (hash TEXT PRIMARY KEY, definition TEXT NOT NULL) STRICT;
CREATE TABLE inventory_schemas (
  namespace TEXT NOT NULL, kind TEXT NOT NULL, version TEXT NOT NULL, definition TEXT NOT NULL,
  PRIMARY KEY(namespace, kind, version)
) STRICT;
CREATE TABLE inventory_sets (
  node_id TEXT NOT NULL REFERENCES service_nodes(id), namespace TEXT NOT NULL, kind TEXT NOT NULL,
  revision INTEGER NOT NULL, schema_version TEXT NOT NULL, observed_at TEXT NOT NULL,
  PRIMARY KEY(node_id, namespace, kind)
) STRICT;
CREATE TABLE inventory (
  node_id TEXT NOT NULL, namespace TEXT NOT NULL, kind TEXT NOT NULL, native_id TEXT NOT NULL,
  schema_version TEXT NOT NULL, summary TEXT NOT NULL, observed_at TEXT NOT NULL,
  PRIMARY KEY(node_id, namespace, kind, native_id),
  FOREIGN KEY(node_id, namespace, kind) REFERENCES inventory_sets(node_id, namespace, kind)
) STRICT;
CREATE TABLE apps (app_id TEXT PRIMARY KEY, metadata TEXT NOT NULL, current_release_id TEXT, previous_release_id TEXT) STRICT;
CREATE TABLE app_releases (
  app_id TEXT NOT NULL REFERENCES apps(app_id), release_id TEXT NOT NULL, release_json TEXT NOT NULL,
  PRIMARY KEY(app_id, release_id)
) STRICT;
CREATE TABLE app_assets (
  app_id TEXT NOT NULL, release_id TEXT NOT NULL, path TEXT NOT NULL, object_id TEXT NOT NULL, revision INTEGER NOT NULL,
  media_type TEXT NOT NULL, content_hash TEXT NOT NULL, PRIMARY KEY(app_id, release_id, path),
  FOREIGN KEY(app_id, release_id) REFERENCES app_releases(app_id, release_id),
  FOREIGN KEY(object_id, revision) REFERENCES revisions(object_id, revision)
) STRICT;
CREATE TABLE deployment_reports (
  host_id TEXT NOT NULL, deployment_id TEXT NOT NULL, node_id TEXT NOT NULL REFERENCES service_nodes(id),
  reported_at TEXT NOT NULL, record_json TEXT NOT NULL, PRIMARY KEY(host_id, deployment_id)
) STRICT;
CREATE TABLE diagnostics (identity TEXT PRIMARY KEY, diagnostic_json TEXT NOT NULL) STRICT;
CREATE TRIGGER object_contract_immutable BEFORE UPDATE OF contract_key ON objects
WHEN NEW.contract_key != OLD.contract_key BEGIN SELECT RAISE(ABORT, 'immutable_contract_key'); END;
CREATE TRIGGER object_owner_immutable BEFORE UPDATE OF owner_object_id ON objects
WHEN NEW.owner_object_id IS NOT OLD.owner_object_id BEGIN SELECT RAISE(ABORT, 'immutable_owner_object_id'); END;
CREATE TRIGGER revision_contract_identity BEFORE INSERT ON revisions
WHEN NEW.contract_key != (SELECT contract_key FROM objects WHERE id=NEW.object_id)
BEGIN SELECT RAISE(ABORT, 'revision_contract_identity'); END;
CREATE TRIGGER revisions_immutable_update BEFORE UPDATE ON revisions BEGIN SELECT RAISE(ABORT, 'immutable_record'); END;
`;

export const immutableTables = [
  "contracts",
  "contract_families",
  "topics",
  "tool_definitions",
  "inventory_schemas",
];
