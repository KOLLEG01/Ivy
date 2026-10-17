PRAGMA user_version=1;
PRAGMA journal_mode=WAL;
PRAGMA synchronous=FULL;
PRAGMA busy_timeout=5000;
CREATE TABLE IF NOT EXISTS identity(singleton INTEGER PRIMARY KEY CHECK(singleton=1), owner_hash TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS attempts(operation_id TEXT PRIMARY KEY, intent TEXT NOT NULL, materialization TEXT NOT NULL,phase TEXT NOT NULL CHECK(phase IN ('prepared','downloading','acquired','failed')), evidence TEXT, payload BLOB, error_code TEXT,CHECK((phase='acquired' AND evidence IS NOT NULL AND payload IS NOT NULL AND error_code IS NULL) OR (phase!='acquired' AND evidence IS NULL AND payload IS NULL)));
CREATE TABLE IF NOT EXISTS read_plans(item_hash TEXT PRIMARY KEY, plan_hash TEXT NOT NULL UNIQUE, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS read_calls(plan_hash TEXT NOT NULL REFERENCES read_plans(plan_hash), slot INTEGER NOT NULL, value TEXT NOT NULL, seen TEXT, observation TEXT, PRIMARY KEY(plan_hash,slot));
