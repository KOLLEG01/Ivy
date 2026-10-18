PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000; PRAGMA auto_vacuum=INCREMENTAL;
      CREATE TABLE IF NOT EXISTS documents(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,parent_id TEXT,current_revision INTEGER NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,UNIQUE(key,name,parent_id));
      CREATE TABLE IF NOT EXISTS revisions(id TEXT NOT NULL,revision INTEGER NOT NULL,value_json TEXT NOT NULL,created_at TEXT NOT NULL,PRIMARY KEY(id,revision));
      CREATE TABLE IF NOT EXISTS blobs(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,parent_id TEXT,bytes BLOB NOT NULL,UNIQUE(key,name,parent_id));
      CREATE TABLE IF NOT EXISTS markers(key TEXT PRIMARY KEY,value TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS cleanup_roots(root_id TEXT PRIMARY KEY,delete_after TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS cleanup_dependencies(root_id TEXT NOT NULL,operation_id TEXT NOT NULL,PRIMARY KEY(root_id,operation_id));
      CREATE TABLE IF NOT EXISTS active_gates(gate_id TEXT PRIMARY KEY,operation_id TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS operation_gates(operation_id TEXT NOT NULL,gate_name TEXT NOT NULL,PRIMARY KEY(operation_id,gate_name));
      CREATE TABLE IF NOT EXISTS workspace_reservations(host_id TEXT NOT NULL,canonical_cwd TEXT NOT NULL,task_id TEXT NOT NULL,operation_id TEXT NOT NULL,PRIMARY KEY(host_id,canonical_cwd),UNIQUE(task_id));
      CREATE TABLE IF NOT EXISTS storage_usage(kind TEXT PRIMARY KEY,rows INTEGER NOT NULL,bytes INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS retired_operations(caller_id TEXT NOT NULL,operation_id TEXT NOT NULL,request_hash TEXT NOT NULL,name TEXT NOT NULL,PRIMARY KEY(caller_id,operation_id));
      CREATE TRIGGER IF NOT EXISTS retired_operation_insert BEFORE INSERT ON documents
        WHEN NEW.key='task-board/operation' AND EXISTS(SELECT 1 FROM retired_operations WHERE name=NEW.name)
        BEGIN SELECT RAISE(ABORT,'operation_expired'); END;
      CREATE UNIQUE INDEX IF NOT EXISTS documents_identity ON documents(key,name,COALESCE(parent_id,''));
      CREATE UNIQUE INDEX IF NOT EXISTS blobs_identity ON blobs(key,name,COALESCE(parent_id,''));
      CREATE INDEX IF NOT EXISTS documents_parent ON documents(parent_id);
      CREATE INDEX IF NOT EXISTS blobs_parent ON blobs(parent_id);
      CREATE INDEX IF NOT EXISTS cleanup_due ON cleanup_roots(delete_after);
      INSERT OR REPLACE INTO active_gates SELECT d.id,json_extract(r.value_json,'$.activeOperation.objectId')
        FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision
        WHERE d.key='task-board/coordination' AND json_type(r.value_json,'$.activeOperation')='object';
      INSERT OR IGNORE INTO storage_usage SELECT 'revisions',COUNT(*),COALESCE(SUM(length(CAST(value_json AS BLOB))),0) FROM revisions;
      INSERT OR IGNORE INTO storage_usage SELECT 'blobs',COUNT(*),COALESCE(SUM(length(bytes)),0) FROM blobs;
      CREATE TRIGGER IF NOT EXISTS revision_usage_insert AFTER INSERT ON revisions BEGIN
        UPDATE storage_usage SET rows=rows+1,bytes=bytes+length(CAST(NEW.value_json AS BLOB)) WHERE kind='revisions';
      END;
      CREATE TRIGGER IF NOT EXISTS revision_usage_delete AFTER DELETE ON revisions BEGIN
        UPDATE storage_usage SET rows=rows-1,bytes=bytes-length(CAST(OLD.value_json AS BLOB)) WHERE kind='revisions';
      END;
      CREATE TRIGGER IF NOT EXISTS blob_usage_insert AFTER INSERT ON blobs BEGIN
        UPDATE storage_usage SET rows=rows+1,bytes=bytes+length(NEW.bytes) WHERE kind='blobs';
      END;
      CREATE TRIGGER IF NOT EXISTS blob_usage_delete AFTER DELETE ON blobs BEGIN
        UPDATE storage_usage SET rows=rows-1,bytes=bytes-length(OLD.bytes) WHERE kind='blobs';
      END;