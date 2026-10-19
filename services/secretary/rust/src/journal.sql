PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;
      CREATE TABLE IF NOT EXISTS documents(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,current_revision INTEGER NOT NULL,created_at INTEGER NOT NULL,completed_at INTEGER,value_json TEXT NOT NULL,UNIQUE(key,name));
      CREATE INDEX IF NOT EXISTS documents_pending ON documents(key,completed_at,id);
      CREATE TABLE IF NOT EXISTS revisions(id TEXT NOT NULL,revision INTEGER NOT NULL,value_json TEXT NOT NULL,PRIMARY KEY(id,revision));
      CREATE TABLE IF NOT EXISTS technical_documents(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,current_revision INTEGER NOT NULL,value_json TEXT NOT NULL,UNIQUE(key,name));
      CREATE TABLE IF NOT EXISTS technical_revisions(id TEXT NOT NULL,revision INTEGER NOT NULL,value_json TEXT NOT NULL,PRIMARY KEY(id,revision));
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
