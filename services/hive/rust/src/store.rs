//! Single-connection storage foundation. Network callers use the storage worker;
//! a Store is created and used only by its database-owning execution context.
use crate::storage_read::STORAGE_FORMAT;
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use rusqlite::{Connection, OptionalExtension, config::DbConfig, params};
use std::{path::Path, rc::Rc, time::Duration};

#[path = "store_backup.rs"]
mod backup;
pub use backup::{BackupSnapshot, OnlineBackup};

#[path = "store_ledger.rs"]
mod ledger;
pub use ledger::{AuthenticatedContext, LedgerResult, ReceiptKind};
#[path = "store_events.rs"]
mod events;
pub use events::EVENT_MAX_COUNT;
#[path = "store_cursor.rs"]
mod cursor;
#[path = "store_query.rs"]
mod query;
pub use query::QueryRuntime;
#[path = "store_failure.rs"]
mod failure;

const SCHEMA: &str = include_str!("schema.sql");
const IMMUTABLE: &[&str] = &[
    "contracts",
    "contract_families",
    "topics",
    "tool_definitions",
    "inventory_schemas",
];
pub(crate) fn sql_error<T>(_: T) -> IvyError {
    IvyError::internal()
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credential {
    pub principal_id: JsString,
    pub digest: JsString,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthContext {
    pub credential_digest: JsString,
    pub session_digest: Option<JsString>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    pub principal_id: JsString,
    pub credential_digest: JsString,
}

pub struct Store {
    db: Rc<Connection>,
    changes: Vec<JsString>,
    cursor_secret: String,
    query_state: query::QueryState,
    failure: failure::FailureCause,
    validators: ivy_contracts::schema::SchemaValidators,
}
/// A failed initializer retains its existing connection through Fatal publication.
/// This never opens a second connection or grants another writable Store.
pub(crate) struct StartupHold {
    _connection: Rc<Connection>,
}
impl Store {
    pub(crate) fn hold_startup_owner(&self) -> StartupHold {
        StartupHold {
            _connection: self.db.clone(),
        }
    }
    pub fn open(path: &Path) -> IvyResult<Self> {
        if path != Path::new(":memory:") {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(sql_error)?;
            }
        }
        let db = Connection::open(path).map_err(sql_error)?;
        db.busy_timeout(Duration::from_secs(1)).map_err(sql_error)?;
        db.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_FKEY, true)
            .map_err(sql_error)?;
        db.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DDL, false)
            .map_err(sql_error)?;
        db.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DML, false)
            .map_err(sql_error)?;
        db.set_prepared_statement_cache_capacity(256);
        let mut store = Self {
            db: Rc::new(db),
            changes: vec![],
            cursor_secret: String::new(),
            query_state: Default::default(),
            failure: Default::default(),
            validators: ivy_contracts::schema::SchemaValidators::new(),
        };
        store.initialize()?;
        store.install_query_functions()?;
        Ok(store)
    }
    /// Consume the storage owner only after its yielding backup tasks finish.
    pub fn close(self) -> IvyResult<()> {
        let connection = Rc::try_unwrap(self.db).map_err(sql_error)?;
        connection.close().map_err(|(_, error)| sql_error(error))
    }
    fn initialize(&mut self) -> IvyResult<()> {
        let format: i64 = self
            .db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(sql_error)?;
        require(
            format == 0 || format == STORAGE_FORMAT,
            "unsupported_storage_format",
            "This binary cannot read the database format.",
        )?;
        // Reject unsupported formats before journal_mode can alter their header.
        self.db.execute_batch("PRAGMA locking_mode=EXCLUSIVE; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=1000;").map_err(sql_error)?;
        if format == 0 {
            self.transaction(|store|{
                // JavaScript template literals normalize source CRLF to LF.
                store.db.execute_batch(&SCHEMA.replace("\r\n","\n")).map_err(sql_error)?;
                for table in IMMUTABLE {for verb in ["UPDATE","DELETE"] {
                    store.db.execute_batch(&format!("CREATE TRIGGER {table}_immutable_{} BEFORE {verb} ON {table} BEGIN SELECT RAISE(ABORT, 'immutable_record'); END;",verb.to_lowercase())).map_err(sql_error)?;
                }}
                let mut secret=[0u8;32];getrandom::fill(&mut secret).map_err(sql_error)?;
                let secret=secret.iter().map(|b|format!("{b:02x}")).collect::<String>();
                let epoch=uuid::Uuid::new_v4().to_string();
                for (key,value) in [("cursor_secret",secret.as_str()),("runtime_epoch",epoch.as_str()),("events_pruned_through","0"),("mutations_expired_before","0"),("mutation_receipt_bytes","0"),("retention_last_success",""),("retention_last_failure","")] {
                    store.db.execute("INSERT INTO metadata VALUES (?, ?)",[key,value]).map_err(sql_error)?;
                }
                store.db.execute_batch("PRAGMA user_version=4").map_err(sql_error)?;
                Ok(())
            })?;
        }
        self.db
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS durable_subscriptions (
      node_id TEXT NOT NULL, name TEXT NOT NULL, PRIMARY KEY(node_id, name),
      FOREIGN KEY(node_id, name) REFERENCES subscriptions(node_id, name)
    ) STRICT",
            )
            .map_err(sql_error)?;
        self.db
            .execute_batch("DROP INDEX IF EXISTS mutations_expiry")
            .map_err(sql_error)?;
        self.cursor_secret = self
            .metadata("cursor_secret")?
            .ok_or_else(IvyError::internal)?;
        self.transaction(|store| {
            let nodes = store
                .db
                .prepare("SELECT id,status_json FROM service_nodes")
                .map_err(sql_error)?
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(sql_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            for (id, status) in nodes {
                let mut status = parse(&status, MANAGEMENT_FRAME_BYTES).map_err(sql_error)?;
                if matches!(status, IvyJson::Null) {
                    return Err(IvyError::internal());
                }
                if matches!(status, IvyJson::Array(_)) {
                    return Err(IvyError::new(
                        "invalid_arguments",
                        "Arrays must be dense and have no extra properties.",
                    ));
                }
                if status.as_object().is_some() {
                    for key in ["connected", "synced", "ready"] {
                        status.insert(key, false.into());
                    }
                }
                store
                    .db
                    .execute(
                        "UPDATE service_nodes SET status_json=? WHERE id=?",
                        params![canonical(&status, MANAGEMENT_FRAME_BYTES)?, id],
                    )
                    .map_err(sql_error)?;
            }
            store
                .db
                .execute_batch("UPDATE subscriptions SET batch_json=NULL")
                .map_err(sql_error)?;
            Ok(())
        })?;
        for (version, operations) in [
            ("1.0.0", vec!["write", "move", "archive"]),
            ("1.1.0", vec!["write", "move", "archive", "delete"]),
        ] {
            let definition = IvyJson::object([
                ("topic", "hive.object.changed".into()),
                ("version", version.into()),
                (
                    "payloadSchema",
                    IvyJson::object([
                        ("type", "object".into()),
                        (
                            "properties",
                            IvyJson::object([
                                ("objectId", IvyJson::object([("type", "string".into())])),
                                (
                                    "revision",
                                    IvyJson::object([
                                        ("type", "integer".into()),
                                        ("minimum", IvyJson::Number(1.0)),
                                    ]),
                                ),
                                (
                                    "operation",
                                    IvyJson::object([(
                                        "enum",
                                        IvyJson::Array(
                                            operations.into_iter().map(IvyJson::from).collect(),
                                        ),
                                    )]),
                                ),
                            ]),
                        ),
                        (
                            "required",
                            IvyJson::Array(
                                ["objectId", "revision", "operation"]
                                    .into_iter()
                                    .map(IvyJson::from)
                                    .collect(),
                            ),
                        ),
                        ("additionalProperties", false.into()),
                    ]),
                ),
            ]);
            self.db
                .execute(
                    "INSERT OR IGNORE INTO topics VALUES (?, ?, ?, ?)",
                    params![
                        "hive.object.changed",
                        version,
                        "hive",
                        canonical(&definition, MANAGEMENT_FRAME_BYTES)?
                    ],
                )
                .map_err(sql_error)?;
        }
        Ok(())
    }
    pub fn connection(&self) -> &Connection {
        &self.db
    }
    pub fn validators(&self) -> &ivy_contracts::schema::SchemaValidators {
        &self.validators
    }
    pub fn metadata(&self, key: &str) -> IvyResult<Option<String>> {
        self.db
            .prepare_cached("SELECT value FROM metadata WHERE key=?")
            .map_err(|e| self.query_error(e))?
            .query_row([key], |r| r.get(0))
            .optional()
            .map_err(|e| self.query_error(e))
    }
    pub fn set_metadata(&self, key: &str, value: &str) -> IvyResult<()> {
        self.db.execute("INSERT INTO metadata VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[key,value]).map_err(|e| self.query_error(e))?;
        Ok(())
    }
    pub fn changes(&self) -> &[JsString] {
        &self.changes
    }
    pub fn take_changes(&mut self) -> Vec<JsString> {
        std::mem::take(&mut self.changes)
    }
    pub fn invalidate(&mut self, scope: JsString) {
        if !self.changes.contains(&scope) {
            self.changes.push(scope);
        }
        if self.changes.len() > 64 {
            self.changes = ["objects", "services", "inventory", "uis", "system"]
                .into_iter()
                .map(JsString::from)
                .collect();
        }
    }
    pub fn transaction<T>(&mut self, work: impl FnOnce(&mut Self) -> IvyResult<T>) -> IvyResult<T> {
        let previous = self.changes.clone();
        self.db
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| self.query_error(e))?;
        let result = work(self).and_then(|value| {
            self.db
                .execute_batch("COMMIT")
                .map_err(|e| self.query_error(e))?;
            Ok(value)
        });
        if result.is_err() {
            if !self.db.is_autocommit() {
                self.db
                    .execute_batch("ROLLBACK")
                    .map_err(|e| self.query_error(e))?;
            }
            self.changes = previous;
        }
        result
    }
    /// Host administration, never exposed as a public network operation.
    pub fn configure_credentials(&mut self, credentials: &[Credential]) -> IvyResult<()> {
        self.transaction(|store|{
            store.db.execute_batch("UPDATE credentials SET revoked=1").map_err(sql_error)?;
            for credential in credentials {
                let principal=text(&credential.principal_id);let digest=text(&credential.digest);
                require(!credential.principal_id.0.is_empty()&&credential.principal_id.0.len()<=200&&digest.len()==71&&digest.starts_with("sha256:")&&digest[7..].bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)),"invalid_arguments","Invalid principal credential configuration.")?;
                store.db.execute("INSERT OR IGNORE INTO principals VALUES (?)",[&principal]).map_err(sql_error)?;
                let existing:Option<String>=store.db.query_row("SELECT principal_id FROM credentials WHERE digest=?",[&digest],|r|r.get(0)).optional().map_err(sql_error)?;
                require(existing.is_none_or(|p|p.encode_utf16().eq(credential.principal_id.0.iter().copied())),"invalid_arguments","A credential cannot change its principal.")?;
                store.db.execute("INSERT INTO credentials VALUES (?, ?, 0) ON CONFLICT(digest) DO UPDATE SET revoked=0",params![digest,principal]).map_err(sql_error)?;
            }
            let principals=store.db.prepare("SELECT DISTINCT principal_id FROM service_nodes").map_err(sql_error)?.query_map([],|r|r.get::<_,String>(0)).map_err(sql_error)?.collect::<Result<Vec<_>,_>>().map_err(sql_error)?;
            for principal in principals {
                if !credentials.iter().any(|c|c.principal_id==JsString::from(principal.as_str())){
                    store.db.execute("DELETE FROM durable_subscriptions WHERE node_id IN (SELECT id FROM service_nodes WHERE principal_id=?)",[&principal]).map_err(sql_error)?;
                    store.db.execute("DELETE FROM subscriptions WHERE node_id IN (SELECT id FROM service_nodes WHERE principal_id=?)",[&principal]).map_err(sql_error)?;
                }
            }
            Ok(())
        })
    }
    pub fn authenticate(&self, context: &AuthContext) -> IvyResult<Principal> {
        let digest = text(&context.credential_digest);
        let principal: Option<String> = self
            .db
            .query_row(
                "SELECT principal_id FROM credentials WHERE digest=? AND revoked=0",
                [&digest],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_error)?;
        let principal = principal.ok_or_else(|| {
            IvyError::new("unauthenticated", "A current Hive credential is required.")
        })?;
        if let Some(session) = context.session_digest.as_ref().filter(|s| !s.0.is_empty()) {
            let found = self
                .db
                .query_row(
                    "SELECT digest FROM browser_sessions WHERE digest=? AND credential_digest=?",
                    params![text(session), digest],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(sql_error)?
                .is_some();
            require(found, "unauthenticated", "Browser session has expired.")?;
        }
        Ok(Principal {
            principal_id: principal.into(),
            credential_digest: context.credential_digest.clone(),
        })
    }
}
