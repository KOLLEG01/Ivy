PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;
CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY, document TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS runs(id TEXT PRIMARY KEY,task_id TEXT NOT NULL,status TEXT NOT NULL,document TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS operations(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL,created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS retired_operations(id TEXT PRIMARY KEY,request_hash TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS retired_runs(id TEXT PRIMARY KEY);
CREATE TRIGGER IF NOT EXISTS retired_operation_insert BEFORE INSERT ON operations
  WHEN EXISTS(SELECT 1 FROM retired_operations WHERE id=NEW.id)
  BEGIN SELECT RAISE(ABORT,'operation_expired'); END;
CREATE INDEX IF NOT EXISTS runs_status ON runs(status);
CREATE INDEX IF NOT EXISTS runs_task_status ON runs(task_id,status);
CREATE INDEX IF NOT EXISTS tasks_job ON tasks(json_extract(document,'$.definition.jobId'));
CREATE INDEX IF NOT EXISTS tasks_due ON tasks(json_extract(document,'$.nextAt'));
CREATE TABLE IF NOT EXISTS task_snapshots(id TEXT PRIMARY KEY,document TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS run_progress(id TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE,event_index INTEGER NOT NULL,error TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS runs_snapshot ON runs(json_extract(document,'$.taskSnapshot'));
CREATE INDEX IF NOT EXISTS tasks_last_run ON tasks(json_extract(document,'$.lastRunId'));

-- Durable accounting follows the owning transaction, including rollback.
BEGIN IMMEDIATE;
CREATE TABLE IF NOT EXISTS journal_usage(kind TEXT PRIMARY KEY,count INTEGER NOT NULL,bytes INTEGER NOT NULL);
INSERT INTO journal_usage SELECT 'progress',(SELECT COUNT(*) FROM run_progress),(SELECT COALESCE(SUM(length(CAST(id AS BLOB))+8+length(CAST(error AS BLOB))),0) FROM run_progress) WHERE NOT EXISTS(SELECT 1 FROM journal_usage WHERE kind='progress');
CREATE TRIGGER IF NOT EXISTS collector_progress_insert AFTER INSERT ON run_progress BEGIN
  UPDATE journal_usage SET count=count+1,bytes=bytes+length(CAST(NEW.id AS BLOB))+8+length(CAST(NEW.error AS BLOB)) WHERE kind='progress';
END;
CREATE TRIGGER IF NOT EXISTS collector_progress_delete AFTER DELETE ON run_progress BEGIN
  UPDATE journal_usage SET count=count-1,bytes=bytes-length(CAST(OLD.id AS BLOB))-8-length(CAST(OLD.error AS BLOB)) WHERE kind='progress';
END;
CREATE TRIGGER IF NOT EXISTS collector_progress_update AFTER UPDATE ON run_progress BEGIN
  UPDATE journal_usage SET bytes=bytes-length(CAST(OLD.error AS BLOB))+length(CAST(NEW.error AS BLOB)) WHERE kind='progress';
END;
INSERT INTO journal_usage SELECT 'runs',(SELECT COUNT(*) FROM runs),(SELECT COALESCE(SUM(length(CAST(document AS BLOB))),0) FROM runs) WHERE NOT EXISTS(SELECT 1 FROM journal_usage WHERE kind='runs');
CREATE TRIGGER IF NOT EXISTS collector_runs_insert AFTER INSERT ON runs BEGIN
  UPDATE journal_usage SET count=count+1,bytes=bytes+length(CAST(NEW.document AS BLOB)) WHERE kind='runs';
END;
CREATE TRIGGER IF NOT EXISTS collector_runs_delete AFTER DELETE ON runs BEGIN
  UPDATE journal_usage SET count=count-1,bytes=bytes-(length(CAST(OLD.document AS BLOB))) WHERE kind='runs';
END;
CREATE TRIGGER IF NOT EXISTS collector_runs_update AFTER UPDATE ON runs BEGIN
  UPDATE journal_usage SET bytes=bytes-(length(CAST(OLD.document AS BLOB)))+(length(CAST(NEW.document AS BLOB))) WHERE kind='runs';
END;
INSERT INTO journal_usage SELECT 'operations',(SELECT COUNT(*) FROM operations),(SELECT COALESCE(SUM(length(CAST(request AS BLOB))+length(CAST(result AS BLOB))),0) FROM operations) WHERE NOT EXISTS(SELECT 1 FROM journal_usage WHERE kind='operations');
CREATE TRIGGER IF NOT EXISTS collector_operations_insert AFTER INSERT ON operations BEGIN
  UPDATE journal_usage SET count=count+1,bytes=bytes+length(CAST(NEW.request AS BLOB))+length(CAST(NEW.result AS BLOB)) WHERE kind='operations';
END;
CREATE TRIGGER IF NOT EXISTS collector_operations_delete AFTER DELETE ON operations BEGIN
  UPDATE journal_usage SET count=count-1,bytes=bytes-(length(CAST(OLD.request AS BLOB))+length(CAST(OLD.result AS BLOB))) WHERE kind='operations';
END;
CREATE TRIGGER IF NOT EXISTS collector_operations_update AFTER UPDATE ON operations BEGIN
  UPDATE journal_usage SET bytes=bytes-(length(CAST(OLD.request AS BLOB))+length(CAST(OLD.result AS BLOB)))+(length(CAST(NEW.request AS BLOB))+length(CAST(NEW.result AS BLOB))) WHERE kind='operations';
END;
INSERT INTO journal_usage SELECT 'snapshots',(SELECT COUNT(*) FROM task_snapshots),(SELECT COALESCE(SUM(length(CAST(document AS BLOB))),0) FROM task_snapshots) WHERE NOT EXISTS(SELECT 1 FROM journal_usage WHERE kind='snapshots');
CREATE TRIGGER IF NOT EXISTS collector_snapshots_insert AFTER INSERT ON task_snapshots BEGIN
  UPDATE journal_usage SET count=count+1,bytes=bytes+length(CAST(NEW.document AS BLOB)) WHERE kind='snapshots';
END;
CREATE TRIGGER IF NOT EXISTS collector_snapshots_delete AFTER DELETE ON task_snapshots BEGIN
  UPDATE journal_usage SET count=count-1,bytes=bytes-(length(CAST(OLD.document AS BLOB))) WHERE kind='snapshots';
END;
CREATE TRIGGER IF NOT EXISTS collector_snapshots_update AFTER UPDATE ON task_snapshots BEGIN
  UPDATE journal_usage SET bytes=bytes-(length(CAST(OLD.document AS BLOB)))+(length(CAST(NEW.document AS BLOB))) WHERE kind='snapshots';
END;
INSERT INTO journal_usage SELECT 'definitions',(SELECT COUNT(*) FROM metadata WHERE key GLOB 'definition-*' AND NOT EXISTS(SELECT 1 FROM retired_operations r WHERE key='definition-request:'||r.id)),(SELECT COALESCE(SUM(length(CAST(value AS BLOB))),0) FROM metadata WHERE key GLOB 'definition-*' AND NOT EXISTS(SELECT 1 FROM retired_operations r WHERE key='definition-request:'||r.id)) WHERE NOT EXISTS(SELECT 1 FROM journal_usage WHERE kind='definitions');
CREATE TRIGGER IF NOT EXISTS collector_definitions_insert AFTER INSERT ON metadata WHEN NEW.key GLOB 'definition-*' AND NOT EXISTS(SELECT 1 FROM retired_operations r WHERE NEW.key='definition-request:'||r.id) BEGIN
  UPDATE journal_usage SET count=count+1,bytes=bytes+length(CAST(NEW.value AS BLOB)) WHERE kind='definitions';
END;
CREATE TRIGGER IF NOT EXISTS collector_definitions_delete AFTER DELETE ON metadata WHEN OLD.key GLOB 'definition-*' AND NOT EXISTS(SELECT 1 FROM retired_operations r WHERE OLD.key='definition-request:'||r.id) BEGIN
  UPDATE journal_usage SET count=count-1,bytes=bytes-length(CAST(OLD.value AS BLOB)) WHERE kind='definitions';
END;
CREATE TRIGGER IF NOT EXISTS collector_definitions_update AFTER UPDATE ON metadata WHEN OLD.key GLOB 'definition-*' OR NEW.key GLOB 'definition-*' BEGIN
  UPDATE journal_usage SET count=count-(CASE WHEN OLD.key GLOB 'definition-*' AND NOT EXISTS(SELECT 1 FROM retired_operations r WHERE OLD.key='definition-request:'||r.id) THEN 1 ELSE 0 END)+(CASE WHEN NEW.key GLOB 'definition-*' AND NOT EXISTS(SELECT 1 FROM retired_operations r WHERE NEW.key='definition-request:'||r.id) THEN 1 ELSE 0 END),
    bytes=bytes-(CASE WHEN OLD.key GLOB 'definition-*' AND NOT EXISTS(SELECT 1 FROM retired_operations r WHERE OLD.key='definition-request:'||r.id) THEN length(CAST(OLD.value AS BLOB)) ELSE 0 END)+(CASE WHEN NEW.key GLOB 'definition-*' AND NOT EXISTS(SELECT 1 FROM retired_operations r WHERE NEW.key='definition-request:'||r.id) THEN length(CAST(NEW.value AS BLOB)) ELSE 0 END) WHERE kind='definitions';
END;
CREATE TRIGGER IF NOT EXISTS collector_retire_definition AFTER INSERT ON retired_operations BEGIN
  UPDATE journal_usage SET count=count-(SELECT COUNT(*) FROM metadata WHERE key='definition-request:'||NEW.id),bytes=bytes-COALESCE((SELECT length(CAST(value AS BLOB)) FROM metadata WHERE key='definition-request:'||NEW.id),0) WHERE kind='definitions';
END;
COMMIT;
