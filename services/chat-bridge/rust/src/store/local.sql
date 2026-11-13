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
CREATE INDEX IF NOT EXISTS documents_parent ON documents(parent_id,id);
CREATE INDEX IF NOT EXISTS blobs_parent ON blobs(parent_id,id);
CREATE INDEX IF NOT EXISTS workflow_cleanup_order ON workflow_cleanup(delete_after);
CREATE TABLE IF NOT EXISTS journal_entry_sizes(kind INTEGER NOT NULL,id TEXT NOT NULL,revision INTEGER NOT NULL,bytes INTEGER NOT NULL,reservation INTEGER NOT NULL,PRIMARY KEY(kind,id,revision));
INSERT OR IGNORE INTO journal_entry_sizes SELECT 0,id,revision,length(CAST(value_json AS BLOB)),CASE WHEN json_extract(value_json,'$.phase') NOT IN ('succeeded','failed') THEN 1048576 ELSE 0 END FROM revisions WHERE NOT EXISTS(SELECT 1 FROM markers WHERE key='journal.metadata.version');
INSERT OR IGNORE INTO journal_entry_sizes SELECT 1,id,1,length(bytes),0 FROM blobs WHERE NOT EXISTS(SELECT 1 FROM markers WHERE key='journal.metadata.version');
CREATE TABLE IF NOT EXISTS journal_usage(id INTEGER PRIMARY KEY CHECK(id=1),entries INTEGER NOT NULL,bytes INTEGER NOT NULL,reserved_bytes INTEGER NOT NULL);
INSERT INTO journal_usage SELECT 1,(SELECT COUNT(*) FROM journal_entry_sizes),COALESCE((SELECT SUM(bytes) FROM journal_entry_sizes),0),COALESCE((SELECT SUM(e.reservation) FROM journal_entry_sizes e JOIN documents d ON e.kind=0 AND d.id=e.id AND d.current_revision=e.revision WHERE d.key='chat-bridge/operation'),0) WHERE NOT EXISTS(SELECT 1 FROM journal_usage WHERE id=1);
CREATE TABLE IF NOT EXISTS journal_metadata(id TEXT NOT NULL,revision INTEGER NOT NULL,document_order INTEGER NOT NULL,retain INTEGER NOT NULL,op_terminal INTEGER NOT NULL,op_blocked INTEGER NOT NULL,operation_id TEXT,issued_at INTEGER,sort_key,action TEXT,notice_hash TEXT,input_id TEXT,reply_id TEXT,offer_id TEXT,expected_main_revision INTEGER,PRIMARY KEY(id,revision));
CREATE INDEX IF NOT EXISTS journal_operation_candidates ON journal_metadata(op_terminal,op_blocked,sort_key,document_order);
CREATE TABLE IF NOT EXISTS journal_references(source_id TEXT NOT NULL,source_revision INTEGER NOT NULL,target_id TEXT NOT NULL,target_revision INTEGER NOT NULL,PRIMARY KEY(source_id,source_revision,target_id,target_revision));
CREATE INDEX IF NOT EXISTS journal_reference_targets ON journal_references(target_id,target_revision);
CREATE TABLE IF NOT EXISTS journal_metadata_pending(id TEXT NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(id,revision));
INSERT OR IGNORE INTO journal_metadata_pending SELECT id,revision FROM revisions WHERE NOT EXISTS(SELECT 1 FROM markers WHERE key='journal.metadata.version');
CREATE TRIGGER IF NOT EXISTS journal_size_insert AFTER INSERT ON journal_entry_sizes BEGIN
  UPDATE journal_usage SET entries=entries+1,bytes=bytes+NEW.bytes,reserved_bytes=reserved_bytes+COALESCE((SELECT NEW.reservation FROM documents WHERE NEW.kind=0 AND id=NEW.id AND current_revision=NEW.revision AND key='chat-bridge/operation'),0) WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS journal_size_update AFTER UPDATE ON journal_entry_sizes BEGIN
  UPDATE journal_usage SET bytes=bytes-OLD.bytes+NEW.bytes,reserved_bytes=reserved_bytes+COALESCE((SELECT NEW.reservation-OLD.reservation FROM documents WHERE NEW.kind=0 AND id=NEW.id AND current_revision=NEW.revision AND key='chat-bridge/operation'),0) WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS journal_size_delete AFTER DELETE ON journal_entry_sizes BEGIN
  UPDATE journal_usage SET entries=entries-1,bytes=bytes-OLD.bytes,reserved_bytes=reserved_bytes-COALESCE((SELECT OLD.reservation FROM documents WHERE OLD.kind=0 AND id=OLD.id AND current_revision=OLD.revision AND key='chat-bridge/operation'),0) WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS journal_revision_insert AFTER INSERT ON revisions BEGIN
  INSERT INTO journal_entry_sizes VALUES(0,NEW.id,NEW.revision,length(CAST(NEW.value_json AS BLOB)),CASE WHEN json_extract(NEW.value_json,'$.phase') NOT IN ('succeeded','failed') THEN 1048576 ELSE 0 END);
  INSERT OR IGNORE INTO journal_metadata_pending VALUES(NEW.id,NEW.revision);
END;
CREATE TRIGGER IF NOT EXISTS journal_revision_update AFTER UPDATE OF value_json ON revisions BEGIN
  UPDATE journal_entry_sizes SET bytes=length(CAST(NEW.value_json AS BLOB)),reservation=CASE WHEN json_extract(NEW.value_json,'$.phase') NOT IN ('succeeded','failed') THEN 1048576 ELSE 0 END WHERE kind=0 AND id=NEW.id AND revision=NEW.revision;
  DELETE FROM journal_metadata WHERE id=NEW.id AND revision=NEW.revision;
  DELETE FROM journal_references WHERE source_id=NEW.id AND source_revision=NEW.revision;
  INSERT OR IGNORE INTO journal_metadata_pending VALUES(NEW.id,NEW.revision);
END;
CREATE TRIGGER IF NOT EXISTS journal_revision_delete AFTER DELETE ON revisions BEGIN
  DELETE FROM journal_entry_sizes WHERE kind=0 AND id=OLD.id AND revision=OLD.revision;
  DELETE FROM journal_metadata WHERE id=OLD.id AND revision=OLD.revision;
  DELETE FROM journal_references WHERE source_id=OLD.id AND source_revision=OLD.revision;
  DELETE FROM journal_metadata_pending WHERE id=OLD.id AND revision=OLD.revision;
END;
CREATE TRIGGER IF NOT EXISTS journal_document_insert AFTER INSERT ON documents BEGIN
  UPDATE journal_usage SET reserved_bytes=reserved_bytes+COALESCE((SELECT reservation FROM journal_entry_sizes WHERE kind=0 AND id=NEW.id AND revision=NEW.current_revision AND NEW.key='chat-bridge/operation'),0) WHERE id=1;
  INSERT OR IGNORE INTO journal_metadata_pending SELECT id,revision FROM revisions WHERE id=NEW.id;
END;
CREATE TRIGGER IF NOT EXISTS journal_document_update AFTER UPDATE OF key,current_revision ON documents BEGIN
  UPDATE journal_usage SET reserved_bytes=reserved_bytes-COALESCE((SELECT reservation FROM journal_entry_sizes WHERE kind=0 AND id=OLD.id AND revision=OLD.current_revision AND OLD.key='chat-bridge/operation'),0)+COALESCE((SELECT reservation FROM journal_entry_sizes WHERE kind=0 AND id=NEW.id AND revision=NEW.current_revision AND NEW.key='chat-bridge/operation'),0) WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS journal_document_key AFTER UPDATE OF key ON documents WHEN NEW.key<>OLD.key BEGIN
  DELETE FROM journal_metadata WHERE id=NEW.id;
  DELETE FROM journal_references WHERE source_id=NEW.id;
  INSERT OR IGNORE INTO journal_metadata_pending SELECT id,revision FROM revisions WHERE id=NEW.id;
END;
CREATE TRIGGER IF NOT EXISTS journal_document_delete AFTER DELETE ON documents BEGIN
  UPDATE journal_usage SET reserved_bytes=reserved_bytes-COALESCE((SELECT reservation FROM journal_entry_sizes WHERE kind=0 AND id=OLD.id AND revision=OLD.current_revision AND OLD.key='chat-bridge/operation'),0) WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS journal_blob_insert AFTER INSERT ON blobs BEGIN
  INSERT INTO journal_entry_sizes VALUES(1,NEW.id,1,length(NEW.bytes),0);
END;
CREATE TRIGGER IF NOT EXISTS journal_blob_update AFTER UPDATE OF bytes ON blobs BEGIN
  UPDATE journal_entry_sizes SET bytes=length(NEW.bytes) WHERE kind=1 AND id=NEW.id AND revision=1;
END;
CREATE TRIGGER IF NOT EXISTS journal_blob_delete AFTER DELETE ON blobs BEGIN
  DELETE FROM journal_entry_sizes WHERE kind=1 AND id=OLD.id AND revision=1;
END;
