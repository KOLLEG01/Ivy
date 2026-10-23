PRAGMA user_version=7;
CREATE TABLE active_calls(call_id TEXT PRIMARY KEY) STRICT;
CREATE TABLE calls(call_id TEXT PRIMARY KEY,operation_id TEXT NOT NULL UNIQUE,value TEXT NOT NULL) STRICT;
CREATE TABLE commands(operation_id TEXT PRIMARY KEY,epoch TEXT NOT NULL,call_id TEXT,method TEXT NOT NULL,phase TEXT NOT NULL,value TEXT NOT NULL) STRICT;
CREATE TABLE epochs(epoch TEXT PRIMARY KEY) STRICT;
CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL) STRICT;
CREATE TABLE receipt_archive(
      kind TEXT NOT NULL,key TEXT NOT NULL,value TEXT NOT NULL,
      PRIMARY KEY(kind,key)
    ) STRICT;
CREATE TABLE receipt_archive_history(sequence INTEGER PRIMARY KEY AUTOINCREMENT,kind TEXT NOT NULL,key TEXT NOT NULL,UNIQUE(kind,key));
CREATE TABLE receipt_archive_retired(kind TEXT NOT NULL,key TEXT NOT NULL,value_hash TEXT NOT NULL,PRIMARY KEY(kind,key)) STRICT;
CREATE TABLE receipt_archive_usage(id INTEGER PRIMARY KEY CHECK(id=1),records INTEGER NOT NULL,bytes INTEGER NOT NULL) STRICT;
CREATE TABLE targets(call_id TEXT PRIMARY KEY,value TEXT NOT NULL) STRICT;
CREATE INDEX commands_by_call ON commands(call_id,method);
CREATE INDEX commands_by_epoch ON commands(epoch,phase);
CREATE INDEX receipt_archive_commands_by_call
        ON receipt_archive(json_extract(value,'$.intent.callId'),json_extract(value,'$.intent.method')) WHERE kind='command';
CREATE TRIGGER receipt_archive_delete AFTER DELETE ON receipt_archive BEGIN
        UPDATE receipt_archive_usage SET records=records-1,bytes=bytes-length(CAST(OLD.value AS BLOB)) WHERE id=1;
      END;
CREATE TRIGGER receipt_archive_insert AFTER INSERT ON receipt_archive BEGIN
        UPDATE receipt_archive_usage SET records=records+1,bytes=bytes+length(CAST(NEW.value AS BLOB)) WHERE id=1;
      END;
CREATE TRIGGER retired_command_insert BEFORE INSERT ON commands
        WHEN EXISTS(SELECT 1 FROM receipt_archive_retired WHERE kind='command' AND key=NEW.operation_id)
        BEGIN SELECT RAISE(ABORT,'operation_expired'); END;
INSERT INTO "commands" VALUES('10000000-0000-4000-8000-000000000005','10000000-0000-4000-8000-000000000011','10000000-0000-4000-8000-000000000012','call.bindVoice','result','{"createdAt":"2026-08-25T10:00:00.000Z","intent":{"callId":"10000000-0000-4000-8000-000000000012","epoch":"10000000-0000-4000-8000-000000000011","method":"call.bindVoice","operationId":"10000000-0000-4000-8000-000000000005","requestHash":"sha256:7d1805cc4b3cba4ef8d3463df385ef08dcd7c3237a31190b130b2dd5860a2ccc","threadId":"10000000-0000-4000-8000-000000000001"},"phase":"result","receipt":{"error":null,"ok":true,"result":{"bound":true,"threadId":"10000000-0000-4000-8000-000000000001"}},"schemaVersion":1,"updatedAt":"2026-08-25T10:00:00.000Z"}');
INSERT INTO "commands" VALUES('10000000-0000-4000-8000-000000000006','10000000-0000-4000-8000-000000000011','10000000-0000-4000-8000-000000000012','call.bindVoice','result','{"createdAt":"2026-08-25T10:00:00.000Z","intent":{"callId":"10000000-0000-4000-8000-000000000012","epoch":"10000000-0000-4000-8000-000000000011","method":"call.bindVoice","operationId":"10000000-0000-4000-8000-000000000006","requestHash":"sha256:e86e2b41fb9fd371b037a213ea4b6c877140643997b7796d7b3ba01b71f7c0f7","threadId":"10000000-0000-4000-8000-000000000003"},"phase":"result","receipt":{"error":null,"ok":false,"result":{"bound":false,"threadId":"10000000-0000-4000-8000-000000000003"}},"schemaVersion":1,"updatedAt":"2026-08-25T10:00:00.000Z"}');
INSERT INTO "commands" VALUES('10000000-0000-4000-8000-000000000007','10000000-0000-4000-8000-000000000011','10000000-0000-4000-8000-000000000012','call.promptVoice','submitted','{"createdAt":"2026-08-25T10:00:00.000Z","intent":{"callId":"10000000-0000-4000-8000-000000000012","epoch":"10000000-0000-4000-8000-000000000011","method":"call.promptVoice","operationId":"10000000-0000-4000-8000-000000000007","requestHash":"sha256:f16d05ec6b29248d2c61adb1e9263f78e4f7bace1b955014a2d17872cfe4064d","threadId":"10000000-0000-4000-8000-000000000001"},"phase":"submitted","receipt":null,"schemaVersion":1,"updatedAt":"2026-08-25T10:00:00.000Z"}');
INSERT INTO "meta" VALUES('owner','{"hostId":"phone-fixture-host","serviceNodeId":"phone-fixture-node"}');
INSERT INTO "meta" VALUES('count','0');
INSERT INTO "meta" VALUES('epochs','0');
INSERT INTO "meta" VALUES('calls','0');
INSERT INTO "meta" VALUES('targets','0');
INSERT INTO "meta" VALUES('voiceReasoning','medium');
INSERT INTO "meta" VALUES('codexArchive:sha256:b3288f1d9732fd9a4a9c39d45b3ca7bec4abef1abccd898e3c2d8fba5052e6ce:10000000-0000-4000-8000-000000000001','{"operationId":null,"threadId":"10000000-0000-4000-8000-000000000001"}');
INSERT INTO "meta" VALUES('codexVoice:prepared-phone-voice:sha256:b3288f1d9732fd9a4a9c39d45b3ca7bec4abef1abccd898e3c2d8fba5052e6ce','{"creationOperationId":null,"fingerprint":"sha256:b3288f1d9732fd9a4a9c39d45b3ca7bec4abef1abccd898e3c2d8fba5052e6ce","threadId":"10000000-0000-4000-8000-000000000001"}');
INSERT INTO "meta" VALUES('codexVoice:pending-phone-voice:sha256:b3288f1d9732fd9a4a9c39d45b3ca7bec4abef1abccd898e3c2d8fba5052e6ce','{"creationOperationId":"10000000-0000-4000-8000-000000000002","fingerprint":"sha256:b3288f1d9732fd9a4a9c39d45b3ca7bec4abef1abccd898e3c2d8fba5052e6ce","threadId":null}');
INSERT INTO "meta" VALUES('codexArchive:sha256:b3288f1d9732fd9a4a9c39d45b3ca7bec4abef1abccd898e3c2d8fba5052e6ce:10000000-0000-4000-8000-000000000003','{"operationId":"10000000-0000-4000-8000-000000000004","threadId":"10000000-0000-4000-8000-000000000003"}');
INSERT INTO "meta" VALUES('voiceRetired:10000000-0000-4000-8000-000000000009','{"deleteOperationId":"10000000-0000-4000-8000-000000000010","phase":"deleting","threadId":"10000000-0000-4000-8000-000000000009"}');
INSERT INTO "receipt_archive" VALUES('command','10000000-0000-4000-8000-000000000008','{"createdAt":"2026-08-25T10:00:00.000Z","intent":{"callId":"10000000-0000-4000-8000-000000000012","epoch":"10000000-0000-4000-8000-000000000011","method":"call.bindVoice","operationId":"10000000-0000-4000-8000-000000000008","requestHash":"sha256:e86e2b41fb9fd371b037a213ea4b6c877140643997b7796d7b3ba01b71f7c0f7","threadId":"10000000-0000-4000-8000-000000000003"},"phase":"result","receipt":{"error":null,"ok":true,"result":{"bound":true,"threadId":"10000000-0000-4000-8000-000000000003"}},"schemaVersion":1,"updatedAt":"2026-08-25T10:00:00.000Z"}');
INSERT INTO "receipt_archive" VALUES('command','10000000-0000-4000-8000-000000000010','{"createdAt":"2026-08-25T10:00:00.000Z","intent":{"callId":"10000000-0000-4000-8000-000000000012","epoch":"10000000-0000-4000-8000-000000000011","method":"call.promptVoice","operationId":"10000000-0000-4000-8000-000000000010","requestHash":"sha256:f16d05ec6b29248d2c61adb1e9263f78e4f7bace1b955014a2d17872cfe4064d","threadId":"10000000-0000-4000-8000-000000000003"},"phase":"outcome_unknown","receipt":null,"schemaVersion":1,"updatedAt":"2026-08-25T10:00:00.000Z"}');
INSERT INTO "receipt_archive_usage" VALUES(1,2,1009);
