PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;
      CREATE TABLE IF NOT EXISTS documents(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,parent_id TEXT,current_revision INTEGER NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,UNIQUE(key,name,parent_id));
      CREATE TABLE IF NOT EXISTS revisions(id TEXT NOT NULL,revision INTEGER NOT NULL,value_json TEXT NOT NULL,created_at TEXT NOT NULL,PRIMARY KEY(id,revision));
      CREATE TABLE IF NOT EXISTS blobs(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,parent_id TEXT,bytes BLOB NOT NULL,UNIQUE(key,name,parent_id));
      CREATE TABLE IF NOT EXISTS markers(key TEXT PRIMARY KEY,value_json TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS workflow_cleanup(root_id TEXT PRIMARY KEY,delete_after INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS offer_acknowledgements(offer_id TEXT NOT NULL,reply_id TEXT NOT NULL,PRIMARY KEY(offer_id,reply_id));
      CREATE TABLE IF NOT EXISTS retired_operations(operation_id TEXT PRIMARY KEY,issued_at INTEGER NOT NULL);
      CREATE TRIGGER IF NOT EXISTS retired_operation_insert BEFORE INSERT ON revisions
        WHEN EXISTS(SELECT 1 FROM documents WHERE id=NEW.id AND key='chat-bridge/operation')
          AND EXISTS(SELECT 1 FROM retired_operations WHERE operation_id=json_extract(NEW.value_json,'$.operationId'))
        BEGIN SELECT RAISE(ABORT,'operation_expired'); END;
      INSERT OR IGNORE INTO offer_acknowledgements SELECT json_extract(r.value_json,'$.request.offerId'),j.value
        FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision,json_each(r.value_json,'$.request.replyIds') j
        WHERE d.key='chat-bridge/operation' AND json_extract(r.value_json,'$.phase')='succeeded'
          AND json_extract(r.value_json,'$.request.action')='acknowledge';
      CREATE UNIQUE INDEX IF NOT EXISTS documents_identity ON documents(key,name,COALESCE(parent_id,''));
      CREATE UNIQUE INDEX IF NOT EXISTS blobs_identity ON blobs(key,name,COALESCE(parent_id,''));
