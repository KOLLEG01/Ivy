PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;
      CREATE TABLE IF NOT EXISTS documents(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,current_revision INTEGER NOT NULL,created_at INTEGER NOT NULL,completed_at INTEGER,value_json TEXT NOT NULL,UNIQUE(key,name));
      CREATE INDEX IF NOT EXISTS documents_pending ON documents(key,completed_at,id);
      CREATE TABLE IF NOT EXISTS revisions(id TEXT NOT NULL,revision INTEGER NOT NULL,value_json TEXT NOT NULL,PRIMARY KEY(id,revision));
      CREATE TABLE IF NOT EXISTS technical_documents(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,current_revision INTEGER NOT NULL,value_json TEXT NOT NULL,UNIQUE(key,name));
      CREATE TABLE IF NOT EXISTS technical_revisions(id TEXT NOT NULL,revision INTEGER NOT NULL,value_json TEXT NOT NULL,PRIMARY KEY(id,revision));
      CREATE INDEX IF NOT EXISTS technical_windows_due ON technical_documents(key,json_extract(value_json,'$.assignmentId'),json_extract(value_json,'$.pending.dueAt')) WHERE key IN ('secretary/object-window','secretary/channel-window');
      CREATE INDEX IF NOT EXISTS technical_follow_up_busy ON technical_documents(json_extract(value_json,'$.threadId'),name) WHERE key='secretary/follow-up' AND COALESCE(json_extract(value_json,'$.phase'),'') NOT IN ('succeeded','failed');
      CREATE INDEX IF NOT EXISTS technical_follow_up_item ON technical_documents(json_extract(value_json,'$.request.itemId'),name) WHERE key='secretary/follow-up';
      CREATE TABLE IF NOT EXISTS technical_blobs(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,bytes BLOB NOT NULL,UNIQUE(key,name));
      CREATE TABLE IF NOT EXISTS technical_cleanup(root_id TEXT PRIMARY KEY,delete_after INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS journal_state(key TEXT PRIMARY KEY,value TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS capture_acknowledgements(capture_id TEXT PRIMARY KEY);
      CREATE TABLE IF NOT EXISTS retired_operations(operation_id TEXT PRIMARY KEY,issued_at INTEGER NOT NULL);
      CREATE TRIGGER IF NOT EXISTS retired_operation_insert BEFORE INSERT ON documents
        WHEN NEW.key='secretary/operation' AND EXISTS(SELECT 1 FROM retired_operations WHERE operation_id=json_extract(NEW.value_json,'$.operationId'))
        BEGIN SELECT RAISE(ABORT,'operation_expired'); END;
      CREATE TRIGGER IF NOT EXISTS retired_technical_operation_insert BEFORE INSERT ON technical_documents
        WHEN EXISTS(SELECT 1 FROM retired_operations WHERE operation_id=COALESCE(json_extract(NEW.value_json,'$.operationId'),json_extract(NEW.value_json,'$.request.operationId')))
        BEGIN SELECT RAISE(ABORT,'operation_expired'); END;

-- One fixed-size accounting row, maintained in the same transaction as its
-- records. Bootstrap old journals once without changing retained contents.
BEGIN IMMEDIATE;
CREATE TABLE IF NOT EXISTS journal_usage(id INTEGER PRIMARY KEY CHECK(id=1),count INTEGER NOT NULL,bytes INTEGER NOT NULL);
INSERT INTO journal_usage
  SELECT 1,
    (SELECT COUNT(*) FROM revisions)+(SELECT COUNT(*) FROM technical_revisions)+(SELECT COUNT(*) FROM technical_blobs)+(SELECT COUNT(*) FROM documents WHERE key='secretary/operation' AND completed_at IS NULL),
    COALESCE((SELECT SUM(length(CAST(value_json AS BLOB))) FROM revisions),0)+COALESCE((SELECT SUM(length(CAST(value_json AS BLOB))) FROM technical_revisions),0)+COALESCE((SELECT SUM(length(bytes)) FROM technical_blobs),0)+COALESCE((SELECT SUM(length(CAST(value_json AS BLOB))+512) FROM documents WHERE key='secretary/operation' AND completed_at IS NULL),0)
  WHERE NOT EXISTS(SELECT 1 FROM journal_usage WHERE id=1);
CREATE TRIGGER IF NOT EXISTS journal_usage_revision_insert AFTER INSERT ON revisions BEGIN
  UPDATE journal_usage SET count=count+1,bytes=bytes+length(CAST(NEW.value_json AS BLOB)) WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS journal_usage_revision_delete AFTER DELETE ON revisions BEGIN
  UPDATE journal_usage SET count=count-1,bytes=bytes-length(CAST(OLD.value_json AS BLOB)) WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS journal_usage_revision_update AFTER UPDATE OF value_json ON revisions BEGIN
  UPDATE journal_usage SET bytes=bytes+length(CAST(NEW.value_json AS BLOB))-length(CAST(OLD.value_json AS BLOB)) WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS journal_usage_technical_revision_insert AFTER INSERT ON technical_revisions BEGIN
  UPDATE journal_usage SET count=count+1,bytes=bytes+length(CAST(NEW.value_json AS BLOB)) WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS journal_usage_technical_revision_delete AFTER DELETE ON technical_revisions BEGIN
  UPDATE journal_usage SET count=count-1,bytes=bytes-length(CAST(OLD.value_json AS BLOB)) WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS journal_usage_technical_revision_update AFTER UPDATE OF value_json ON technical_revisions BEGIN
  UPDATE journal_usage SET bytes=bytes+length(CAST(NEW.value_json AS BLOB))-length(CAST(OLD.value_json AS BLOB)) WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS journal_usage_blob_insert AFTER INSERT ON technical_blobs BEGIN
  UPDATE journal_usage SET count=count+1,bytes=bytes+length(NEW.bytes) WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS journal_usage_blob_delete AFTER DELETE ON technical_blobs BEGIN
  UPDATE journal_usage SET count=count-1,bytes=bytes-length(OLD.bytes) WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS journal_usage_blob_update AFTER UPDATE OF bytes ON technical_blobs BEGIN
  UPDATE journal_usage SET bytes=bytes+length(NEW.bytes)-length(OLD.bytes) WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS journal_usage_pending_insert AFTER INSERT ON documents
  WHEN NEW.key='secretary/operation' AND NEW.completed_at IS NULL BEGIN
  UPDATE journal_usage SET count=count+1,bytes=bytes+length(CAST(NEW.value_json AS BLOB))+512 WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS journal_usage_pending_delete AFTER DELETE ON documents
  WHEN OLD.key='secretary/operation' AND OLD.completed_at IS NULL BEGIN
  UPDATE journal_usage SET count=count-1,bytes=bytes-length(CAST(OLD.value_json AS BLOB))-512 WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS journal_usage_pending_update AFTER UPDATE OF key,completed_at,value_json ON documents
  WHEN (OLD.key='secretary/operation' AND OLD.completed_at IS NULL) OR (NEW.key='secretary/operation' AND NEW.completed_at IS NULL) BEGIN
  UPDATE journal_usage SET
    count=count+(CASE WHEN NEW.key='secretary/operation' AND NEW.completed_at IS NULL THEN 1 ELSE 0 END)-(CASE WHEN OLD.key='secretary/operation' AND OLD.completed_at IS NULL THEN 1 ELSE 0 END),
    bytes=bytes+(CASE WHEN NEW.key='secretary/operation' AND NEW.completed_at IS NULL THEN length(CAST(NEW.value_json AS BLOB))+512 ELSE 0 END)-(CASE WHEN OLD.key='secretary/operation' AND OLD.completed_at IS NULL THEN length(CAST(OLD.value_json AS BLOB))+512 ELSE 0 END)
  WHERE id=1;
END;
COMMIT;
