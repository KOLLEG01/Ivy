//! Session persistence derived from the pinned adapter (MIT; see
//! `services/agent-manager/claude/LICENSE.adapter`).
//!
//! Callers serialize access and supply the selected adapter home and clock.
//! There is no account-home discovery or provider dependency in this owner.
use ivy_contracts::{
    IvyJson, JsString,
    json::{js_string, parse, stringify},
};
use ivy_store::rusqlite::{
    self, Connection, OpenFlags,
    config::DbConfig,
    params_from_iter,
    types::{Value, ValueRef},
};
use std::{
    fmt, fs,
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const PINNED_SECTION_ID: &str = "01984de2-8f74-7c91-a3b2-5c5e937cf318";
pub const PINNED_SECTION_NAME: &str = "Pinned";
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
pub type Result<T> = std::result::Result<T, StoreError>;
static NULL: IvyJson = IvyJson::Null;

#[derive(Debug)]
pub struct StoreError {
    pub name: &'static str,
    pub message: String,
    pub code: Option<&'static str>,
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for StoreError {}
impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        let message = match error {
            rusqlite::Error::SqliteFailure(_, Some(message)) => message,
            other => other.to_string(),
        };
        Self {
            name: "Error",
            message,
            code: Some("ERR_SQLITE_ERROR"),
        }
    }
}
fn error(message: impl Into<String>) -> StoreError {
    StoreError {
        name: "Error",
        message: message.into(),
        code: None,
    }
}
fn item_array<'a>(turn: &'a IvyJson, method: &str) -> Result<&'a [IvyJson]> {
    let items = field(turn, "items");
    items.as_array().ok_or_else(|| StoreError {
        name: "TypeError",
        message: if items.is_null() {
            format!("Cannot read properties of null (reading '{method}')")
        } else {
            format!("turn.items.{method} is not a function")
        },
        code: None,
    })
}
fn json_error(error: ivy_contracts::IvyError) -> StoreError {
    StoreError {
        name: "SyntaxError",
        message: error.message,
        code: None,
    }
}
fn encode(value: &IvyJson) -> Result<String> {
    stringify(value, usize::MAX).map_err(json_error)
}
fn decode(value: &IvyJson) -> Result<IvyJson> {
    parse(&text(value), usize::MAX).map_err(json_error)
}
fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.get(key).unwrap_or(&NULL)
}
fn text(value: &IvyJson) -> String {
    String::from_utf16_lossy(&js_string(value).0)
}
fn is(value: &IvyJson, expected: &str) -> bool {
    value
        .as_js_string()
        .is_some_and(|v| v == &JsString::from(expected))
}
pub(super) fn truthy(value: &IvyJson) -> bool {
    match value {
        IvyJson::Null => false,
        IvyJson::Bool(v) => *v,
        IvyJson::Number(v) => *v != 0.0 && !v.is_nan(),
        IvyJson::String(v) => !v.0.is_empty(),
        _ => true,
    }
}
pub(super) fn number(value: &IvyJson) -> f64 {
    // SQLite affinity normally supplies numbers. Preserve legacy primitive
    // coercion for hand-created rows; this is not a protocol input parser.
    match value {
        IvyJson::Number(v) => *v,
        IvyJson::Null => 0.0,
        IvyJson::Bool(v) => {
            if *v {
                1.0
            } else {
                0.0
            }
        }
        _ => {
            let units = js_string(value).trim();
            let Ok(s) = units.to_utf8() else {
                return f64::NAN;
            };
            if s.is_empty() {
                return 0.0;
            }
            if matches!(s.as_str(), "Infinity" | "+Infinity") {
                return f64::INFINITY;
            }
            if s == "-Infinity" {
                return f64::NEG_INFINITY;
            }
            if let Some(digits) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                return radix(digits, 16);
            }
            if let Some(digits) = s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")) {
                return radix(digits, 8);
            }
            if let Some(digits) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
                return radix(digits, 2);
            }
            if !s
                .bytes()
                .all(|c| c.is_ascii_digit() || b"+-.eE".contains(&c))
            {
                return f64::NAN;
            }
            s.parse().unwrap_or(f64::NAN)
        }
    }
}
fn radix(s: &str, base: u32) -> f64 {
    if s.is_empty() {
        return f64::NAN;
    }
    let width = base.trailing_zeros();
    let (mut significand, mut bits, mut guard, mut sticky) = (0u64, 0usize, false, false);
    for c in s.chars() {
        let Some(digit) = c.to_digit(base) else {
            return f64::NAN;
        };
        for shift in (0..width).rev() {
            let bit = (digit >> shift) & 1;
            if bits == 0 && bit == 0 {
                continue;
            }
            bits = bits.saturating_add(1);
            if bits <= 53 {
                significand = (significand << 1) | u64::from(bit);
            } else if bits == 54 {
                guard = bit != 0;
            } else {
                sticky |= bit != 0;
            }
        }
    }
    if bits <= 53 {
        return significand as f64;
    }
    if guard && (sticky || significand & 1 != 0) {
        significand += 1;
    }
    (significand as f64) * 2f64.powi(bits.saturating_sub(53).min(1024) as i32)
}
fn nullable_number(value: &IvyJson) -> IvyJson {
    if value.is_null() {
        IvyJson::Null
    } else {
        IvyJson::Number(number(value))
    }
}
fn nullable_text(value: &IvyJson) -> IvyJson {
    if value.is_null() {
        IvyJson::Null
    } else {
        IvyJson::String(js_string(value))
    }
}
fn coalesce(value: &IvyJson, fallback: &IvyJson) -> IvyJson {
    if value.is_null() {
        fallback.clone()
    } else {
        value.clone()
    }
}
fn require_bindings(value: &IvyJson, fields: &[(&str, usize)]) -> Result<()> {
    for (key, position) in fields {
        if value.get(key).is_none() {
            return Err(StoreError {
                name: "TypeError",
                message: format!("Provided value cannot be bound to SQLite parameter {position}."),
                code: Some("ERR_INVALID_ARG_TYPE"),
            });
        }
    }
    Ok(())
}
fn nonnegative(value: f64) -> f64 {
    if value.is_nan() {
        value
    } else {
        value.max(0.0)
    }
}

fn bind(values: &[IvyJson]) -> Result<Vec<Value>> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            Ok(match value {
                IvyJson::Null => Value::Null,
                IvyJson::Number(v) => Value::Real(*v),
                IvyJson::String(v) => Value::Text(String::from_utf16_lossy(&v.0)),
                _ => {
                    return Err(StoreError {
                        name: "TypeError",
                        message: format!(
                            "Provided value cannot be bound to SQLite parameter {}.",
                            index + 1
                        ),
                        code: Some("ERR_INVALID_ARG_TYPE"),
                    });
                }
            })
        })
        .collect()
}
fn cell(value: ValueRef<'_>) -> Result<IvyJson> {
    Ok(match value {
        ValueRef::Null => IvyJson::Null,
        ValueRef::Integer(v) => {
            if !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&v) {
                return Err(StoreError {
                    name: "RangeError",
                    message: format!(
                        "Value is too large to be represented as a JavaScript number: {v}"
                    ),
                    code: Some("ERR_OUT_OF_RANGE"),
                });
            }
            IvyJson::Number(v as f64)
        }
        ValueRef::Real(v) => IvyJson::Number(v),
        ValueRef::Text(v) => String::from_utf8_lossy(v).into_owned().into(),
        // String(Uint8Array) at the row projection boundary.
        ValueRef::Blob(v) => v
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(",")
            .into(),
    })
}

/// Options are already decoded by the native request handler. Cursors and
/// limits retain f64/NaN/infinity until SQLite, as in the original store.
#[derive(Default)]
pub struct ThreadList {
    pub archived: bool,
    pub limit: Option<f64>,
    pub cursor: Option<f64>,
    pub is_pinned: Option<bool>,
    pub cwd: Vec<JsString>,
    pub include_ephemeral: bool,
    pub parent_thread_id: Option<JsString>,
    pub ancestor_thread_id: Option<JsString>,
    pub source_kinds: Vec<JsString>,
    /// None omits the filter; Some(None) selects threads with no section.
    pub section_id: Option<Option<JsString>>,
    pub sort_key: Option<ThreadSort>,
    pub ascending: bool,
}
#[derive(Clone, Copy)]
pub enum ThreadSort {
    Created,
    Updated,
    Recency,
    SectionPosition,
}

pub struct SessionStore {
    db: Connection,
    clock: Clock,
}
impl SessionStore {
    pub fn open(home: &Path, path: &Path) -> Result<Self> {
        Self::with_clock(
            home,
            path,
            Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or_else(|e| -e.duration().as_secs_f64(), |d| d.as_secs_f64())
                    .floor()
            }),
        )
    }
    pub fn with_clock(home: &Path, path: &Path, clock: Clock) -> Result<Self> {
        let mut directory = fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        directory.create(home).map_err(|e| error(e.to_string()))?;
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.busy_timeout(Duration::ZERO)?;
        db.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_FKEY, true)?;
        db.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DDL, false)?;
        db.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DML, false)?;
        let store = Self { db, clock };
        store.migrate()?;
        Ok(store)
    }
    fn run(&self, sql: &str, values: &[IvyJson]) -> Result<usize> {
        Ok(self.db.execute(sql, params_from_iter(bind(values)?))?)
    }
    fn all(&self, sql: &str, values: &[IvyJson]) -> Result<Vec<IvyJson>> {
        let mut statement = self.db.prepare(sql)?;
        let names = statement
            .column_names()
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let mut rows = statement.query(params_from_iter(bind(values)?))?;
        let mut output = Vec::new();
        while let Some(row) = rows.next()? {
            let mut values = Vec::with_capacity(names.len());
            for (i, name) in names.iter().enumerate() {
                values.push((name.clone(), cell(row.get_ref(i)?)?));
            }
            output.push(IvyJson::object(values));
        }
        Ok(output)
    }
    fn one(&self, sql: &str, values: &[IvyJson]) -> Result<Option<IvyJson>> {
        Ok(self.all(sql, values)?.into_iter().next())
    }
    fn now(&self) -> IvyJson {
        IvyJson::Number((self.clock)())
    }
    fn migrate(&self) -> Result<()> {
        self.db.execute_batch(include_str!("store_schema.sql"))?;
        for (column, definition) in [
            ("is_pinned", "INTEGER NOT NULL DEFAULT 0"),
            ("section_id", "TEXT"),
            ("section_entered_at", "INTEGER"),
            ("section_position", "INTEGER"),
        ] {
            self.ensure_column(column, definition)?;
        }
        self.db.execute_batch("CREATE INDEX IF NOT EXISTS idx_threads_section_position ON threads(section_id, section_position, id)")?;
        for (column, definition) in [
            ("reasoning_effort", "TEXT"),
            ("approval_policy", "TEXT"),
            ("sandbox_mode", "TEXT"),
            ("permission_profile_id", "TEXT"),
            ("collaboration_mode", "TEXT NOT NULL DEFAULT 'default'"),
            ("ephemeral", "INTEGER NOT NULL DEFAULT 0"),
            ("thread_source", "TEXT"),
            ("agent_role", "TEXT"),
            ("agent_nickname", "TEXT"),
            ("base_instructions", "TEXT"),
            ("developer_instructions", "TEXT"),
            ("personality", "TEXT"),
            ("runtime_backend", "TEXT NOT NULL DEFAULT 'claude'"),
            ("codex_session_id", "TEXT"),
        ] {
            self.ensure_column(column, definition)?;
        }
        let now = self.now();
        self.run("INSERT OR IGNORE INTO thread_sections\n         (id, name, appearance_json, created_at, updated_at)\n         VALUES (?, ?, NULL, ?, ?)",&[PINNED_SECTION_ID.into(),PINNED_SECTION_NAME.into(),now.clone(),now])?;
        self.run("UPDATE threads SET section_id = ?, section_entered_at = COALESCE(section_entered_at, updated_at), section_position = COALESCE(section_position, rowid) WHERE is_pinned = 1 AND (section_id IS NULL OR section_id = '')",&[PINNED_SECTION_ID.into()])?;
        self.run("UPDATE threads SET is_pinned = CASE WHEN section_id = ? THEN 1 ELSE 0 END WHERE section_id IS NOT NULL OR is_pinned = 1",&[PINNED_SECTION_ID.into()])?;
        self.db.execute_batch("UPDATE threads SET section_entered_at = NULL, section_position = NULL WHERE section_id IS NULL")?;
        let _=self.db.execute_batch("UPDATE threads SET thread_source = NULL WHERE thread_source IS NOT NULL AND thread_source NOT IN ('user', 'subagent', 'memory_consolidation');
        UPDATE threads SET agent_role = NULL WHERE agent_role = '';
        UPDATE threads SET agent_nickname = NULL WHERE agent_nickname = '';
        UPDATE threads SET base_instructions = NULL WHERE base_instructions = '';
        UPDATE threads SET developer_instructions = NULL WHERE developer_instructions = '';
        UPDATE threads SET source = 'appServer' WHERE source = 'app_server';");
        Ok(())
    }
    fn ensure_column(&self, column: &str, definition: &str) -> Result<()> {
        if !self
            .all("PRAGMA table_info(threads)", &[])?
            .iter()
            .any(|row| is(field(row, "name"), column))
        {
            self.db.execute_batch(&format!(
                "ALTER TABLE threads ADD COLUMN {column} {definition}"
            ))?;
        }
        Ok(())
    }
    pub fn upsert_thread(&self, thread: &IvyJson) -> Result<()> {
        require_bindings(
            thread,
            &[
                ("id", 1),
                ("sessionId", 2),
                ("forkedFromId", 3),
                ("preview", 8),
                ("name", 9),
                ("cwd", 11),
                ("model", 12),
                ("reasoningEffort", 13),
                ("modelProvider", 14),
                ("claudeSessionId", 15),
                ("source", 16),
                ("createdAt", 17),
                ("updatedAt", 18),
                ("status", 19),
                ("approvalPolicy", 20),
                ("sandboxMode", 21),
                ("threadSource", 24),
                ("agentRole", 25),
                ("agentNickname", 26),
                ("baseInstructions", 27),
                ("developerInstructions", 28),
                ("personality", 29),
                ("runtimeBackend", 30),
                ("codexSessionId", 31),
            ],
        )?;
        let section = thread.get("sectionId").cloned().unwrap_or_else(|| {
            if field(thread, "isPinned") == &IvyJson::Bool(true) {
                PINNED_SECTION_ID.into()
            } else {
                IvyJson::Null
            }
        });
        let pinned = is(&section, PINNED_SECTION_ID);
        let entered = if section.is_null() {
            IvyJson::Null
        } else {
            coalesce(
                field(thread, "sectionEnteredAt"),
                field(thread, "updatedAt"),
            )
        };
        let position = if section.is_null() {
            IvyJson::Null
        } else {
            field(thread, "sectionPosition").clone()
        };
        let columns = [
            "id",
            "session_id",
            "forked_from_id",
            "is_pinned",
            "section_id",
            "section_entered_at",
            "section_position",
            "preview",
            "name",
            "archived",
            "cwd",
            "model",
            "reasoning_effort",
            "model_provider",
            "claude_session_id",
            "source",
            "created_at",
            "updated_at",
            "status_json",
            "approval_policy",
            "sandbox_mode",
            "permission_profile_id",
            "ephemeral",
            "thread_source",
            "agent_role",
            "agent_nickname",
            "base_instructions",
            "developer_instructions",
            "personality",
            "runtime_backend",
            "codex_session_id",
            "collaboration_mode",
        ];
        let mut values = Vec::with_capacity(columns.len());
        for key in ["id", "sessionId", "forkedFromId"] {
            values.push(field(thread, key).clone());
        }
        values.extend([
            IvyJson::Number(if pinned { 1.0 } else { 0.0 }),
            section,
            entered,
            position,
        ]);
        for key in ["preview", "name"] {
            values.push(field(thread, key).clone());
        }
        values.push(IvyJson::Number(if truthy(field(thread, "archived")) {
            1.0
        } else {
            0.0
        }));
        for key in [
            "cwd",
            "model",
            "reasoningEffort",
            "modelProvider",
            "claudeSessionId",
            "source",
            "createdAt",
            "updatedAt",
        ] {
            values.push(field(thread, key).clone());
        }
        values.push(encode(field(thread, "status"))?.into());
        for key in ["approvalPolicy", "sandboxMode", "permissionProfileId"] {
            values.push(field(thread, key).clone());
        }
        values.push(IvyJson::Number(if truthy(field(thread, "ephemeral")) {
            1.0
        } else {
            0.0
        }));
        for key in [
            "threadSource",
            "agentRole",
            "agentNickname",
            "baseInstructions",
            "developerInstructions",
            "personality",
            "runtimeBackend",
            "codexSessionId",
        ] {
            values.push(field(thread, key).clone());
        }
        values.push(coalesce(
            field(thread, "collaborationMode"),
            &"default".into(),
        ));
        let assignments = columns
            .iter()
            .filter(|c| !matches!(**c, "id" | "created_at"))
            .map(|c| format!("{c}=excluded.{c}"))
            .collect::<Vec<_>>()
            .join(",");
        self.run(
            &format!(
                "INSERT INTO threads ({}) VALUES ({}) ON CONFLICT(id) DO UPDATE SET {assignments}",
                columns.join(","),
                vec!["?"; columns.len()].join(",")
            ),
            &values,
        )?;
        Ok(())
    }
    pub fn get_thread(&self, id: &JsString) -> Result<Option<IvyJson>> {
        self.one(
            "SELECT * FROM threads WHERE id = ?",
            &[IvyJson::String(id.clone())],
        )?
        .map(|row| row_to_thread(&row))
        .transpose()
    }
    pub fn list_threads(&self, options: &ThreadList) -> Result<Vec<IvyJson>> {
        let limit = options.limit.unwrap_or(50.0);
        let limit = if limit.is_nan() {
            limit
        } else {
            limit.clamp(1.0, 200.0)
        };
        let expression = match options.sort_key.unwrap_or(ThreadSort::Created) {
            ThreadSort::Created => "t.created_at",
            ThreadSort::Updated | ThreadSort::Recency => "t.updated_at",
            ThreadSort::SectionPosition => "COALESCE(t.section_position, t.created_at)",
        };
        let direction = if options.ascending { "ASC" } else { "DESC" };
        let mut conditions = vec![
            "t.archived = ?".to_string(),
            format!(
                "{expression} {} ?",
                if options.ascending { ">" } else { "<" }
            ),
        ];
        let mut args = vec![
            IvyJson::Number(if options.archived { 1.0 } else { 0.0 }),
            IvyJson::Number(options.cursor.unwrap_or(if options.ascending {
                -1.0
            } else {
                9_007_199_254_740_991.0
            })),
        ];
        if let Some(pinned) = options.is_pinned {
            conditions.push("t.is_pinned = ?".into());
            args.push(IvyJson::Number(if pinned { 1.0 } else { 0.0 }));
        }
        if let Some(section) = &options.section_id {
            match section {
                None => conditions.push("t.section_id IS NULL".into()),
                Some(id) => {
                    conditions.push("t.section_id = ?".into());
                    args.push(IvyJson::String(id.clone()));
                }
            }
        }
        if let Some(parent) = &options.parent_thread_id {
            conditions.push("t.forked_from_id = ?".into());
            args.push(IvyJson::String(parent.clone()));
        }
        if options.ancestor_thread_id.is_some() {
            conditions.push("t.id IN (SELECT id FROM descendants)".into());
        }
        if !options.source_kinds.is_empty() {
            let predicates = options
                .source_kinds
                .iter()
                .filter_map(|kind| match kind.to_utf8().ok().as_deref() {
                    Some("subAgent" | "subAgentThreadSpawn") => Some(
                        "(t.thread_source = 'subagent' AND t.forked_from_id IS NOT NULL)"
                            .to_string(),
                    ),
                    Some("user") => Some("t.thread_source = 'user'".into()),
                    Some("memoryConsolidation") => {
                        Some("t.thread_source = 'memory_consolidation'".into())
                    }
                    Some(kind @ ("appServer" | "cli" | "exec" | "vscode" | "unknown")) => {
                        Some(format!("t.source = '{kind}'"))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            conditions.push(if predicates.is_empty() {
                "1 = 0".into()
            } else {
                format!("({})", predicates.join(" OR "))
            });
        }
        let topology = options.parent_thread_id.is_some()
            || options.ancestor_thread_id.is_some()
            || options.source_kinds.iter().any(|s| {
                matches!(
                    s.to_utf8().ok().as_deref(),
                    Some("subAgent" | "subAgentThreadSpawn")
                )
            });
        if !options.include_ephemeral && !topology {
            conditions.push("t.ephemeral = 0".into());
        }
        if !options.cwd.is_empty() {
            conditions.push(format!(
                "t.cwd IN ({})",
                vec!["?"; options.cwd.len()].join(",")
            ));
            args.extend(options.cwd.iter().cloned().map(IvyJson::String));
        }
        let cte = if let Some(ancestor) = &options.ancestor_thread_id {
            args.insert(0, IvyJson::String(ancestor.clone()));
            "WITH RECURSIVE descendants(id) AS (SELECT id FROM threads WHERE forked_from_id = ? UNION SELECT child.id FROM threads child JOIN descendants parent ON child.forked_from_id = parent.id)"
        } else {
            ""
        };
        args.push(IvyJson::Number(limit));
        self.all(&format!("{cte} SELECT t.* FROM threads t WHERE {} ORDER BY {expression} {direction}, t.id {direction} LIMIT ?",conditions.join(" AND ")),&args)?.iter().map(row_to_thread).collect()
    }
    pub fn list_sections(&self, limit: f64, offset: f64) -> Result<Vec<IvyJson>> {
        let limit = if limit == 0.0 || limit.is_nan() {
            100.0
        } else {
            limit.clamp(1.0, 200.0)
        };
        let offset = if offset.is_nan() {
            0.0
        } else {
            offset.max(0.0)
        };
        self.all("SELECT * FROM thread_sections ORDER BY CASE WHEN id = ? THEN 0 ELSE 1 END, created_at ASC, id ASC LIMIT ? OFFSET ?",&[PINNED_SECTION_ID.into(),IvyJson::Number(limit),IvyJson::Number(offset)])?.iter().map(row_to_section).collect()
    }
    pub fn get_section(&self, id: &JsString) -> Result<Option<IvyJson>> {
        if let Some(row) = self.one(
            "SELECT * FROM thread_sections WHERE id = ?",
            &[IvyJson::String(id.clone())],
        )? {
            return Ok(Some(row_to_section(&row)?));
        }
        Ok((id == &JsString::from(PINNED_SECTION_ID)).then(|| {
            IvyJson::object([
                ("id", PINNED_SECTION_ID.into()),
                ("name", PINNED_SECTION_NAME.into()),
                ("appearance", IvyJson::Null),
            ])
        }))
    }
    pub fn create_section(
        &self,
        id: &JsString,
        name: &JsString,
        appearance: &IvyJson,
    ) -> Result<IvyJson> {
        let name = name.trim();
        if name.0.is_empty() {
            return Err(error("section name must not be empty"));
        }
        if id == &JsString::from(PINNED_SECTION_ID) {
            return Err(error("the pinned section is reserved"));
        }
        let now = self.now();
        self.run("INSERT INTO thread_sections (id, name, appearance_json, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",&[IvyJson::String(id.clone()),IvyJson::String(name),if appearance.is_null() {IvyJson::Null} else {encode(appearance)?.into()},now.clone(),now])?;
        self.get_section(id)?.ok_or_else(|| {
            error(format!(
                "unknown section: {}",
                text(&IvyJson::String(id.clone()))
            ))
        })
    }
    pub fn update_section(
        &self,
        id: &JsString,
        name: &JsString,
        appearance: &IvyJson,
    ) -> Result<IvyJson> {
        if self.get_section(id)?.is_none() {
            return Err(error(format!(
                "unknown section: {}",
                text(&IvyJson::String(id.clone()))
            )));
        }
        let name = if id == &JsString::from(PINNED_SECTION_ID) {
            PINNED_SECTION_NAME.into()
        } else {
            name.trim()
        };
        if name.0.is_empty() {
            return Err(error("section name must not be empty"));
        }
        self.run(
            "UPDATE thread_sections SET name = ?, appearance_json = ?, updated_at = ? WHERE id = ?",
            &[
                IvyJson::String(name),
                if appearance.is_null() {
                    IvyJson::Null
                } else {
                    encode(appearance)?.into()
                },
                self.now(),
                IvyJson::String(id.clone()),
            ],
        )?;
        self.get_section(id)?.ok_or_else(|| {
            error(format!(
                "unknown section: {}",
                text(&IvyJson::String(id.clone()))
            ))
        })
    }
    fn transaction<T>(&self, run: impl FnOnce() -> Result<T>) -> Result<T> {
        self.db.execute_batch("BEGIN IMMEDIATE")?;
        let result = run().and_then(|value| {
            self.db.execute_batch("COMMIT")?;
            Ok(value)
        });
        if result.is_err() {
            let _ = self.db.execute_batch("ROLLBACK");
        }
        result
    }
    pub fn delete_section(&self, id: &JsString) -> Result<()> {
        if id == &JsString::from(PINNED_SECTION_ID) {
            return Err(error("the pinned section is reserved"));
        }
        if self.get_section(id)?.is_none() {
            return Err(error(format!(
                "unknown section: {}",
                text(&IvyJson::String(id.clone()))
            )));
        }
        self.transaction(||{
            self.run("UPDATE threads SET section_id = NULL, section_entered_at = NULL, section_position = NULL, is_pinned = 0 WHERE section_id = ?",&[IvyJson::String(id.clone())])?;
            self.run("DELETE FROM thread_sections WHERE id = ?",&[IvyJson::String(id.clone())])?;Ok(())
        })
    }
    fn section_thread_ids(
        &self,
        section: Option<&JsString>,
        exclude: &JsString,
    ) -> Result<Vec<JsString>> {
        let (condition, args) = if let Some(id) = section {
            (
                "section_id = ?",
                vec![
                    IvyJson::String(id.clone()),
                    IvyJson::String(exclude.clone()),
                ],
            )
        } else {
            ("section_id IS NULL", vec![IvyJson::String(exclude.clone())])
        };
        Ok(self.all(&format!("SELECT id FROM threads WHERE {condition} AND id <> ? ORDER BY COALESCE(section_position, created_at), updated_at, id"),&args)?.iter().map(|r|js_string(field(r,"id"))).collect())
    }
    fn renumber_section(&self, ids: &[JsString]) -> Result<()> {
        for (position, id) in ids.iter().enumerate() {
            self.run(
                "UPDATE threads SET section_position = ? WHERE id = ?",
                &[
                    IvyJson::Number(position as f64),
                    IvyJson::String(id.clone()),
                ],
            )?;
        }
        Ok(())
    }
    pub fn move_thread_to_section(
        &self,
        id: &JsString,
        section: Option<&JsString>,
        before: Option<&JsString>,
    ) -> Result<IvyJson> {
        let unknown = || {
            error(format!(
                "unknown thread: {}",
                text(&IvyJson::String(id.clone()))
            ))
        };
        let thread = self.get_thread(id)?.ok_or_else(unknown)?;
        if let Some(section) = section {
            if self.get_section(section)?.is_none() {
                return Err(error(format!(
                    "unknown section: {}",
                    text(&IvyJson::String(section.clone()))
                )));
            }
        }
        let old = field(&thread, "sectionId").as_js_string();
        let old_ids = self.section_thread_ids(old, id)?;
        let mut target_ids = if section.is_some() {
            self.section_thread_ids(section, id)?
        } else {
            Vec::new()
        };
        let position = if section.is_some() {
            Some(
                before
                    .and_then(|b| target_ids.iter().position(|v| v == b))
                    .unwrap_or(target_ids.len()),
            )
        } else {
            None
        };
        if let Some(position) = position {
            target_ids.insert(position, id.clone());
        }
        let entered = if section.is_none() {
            IvyJson::Null
        } else if old == section && !field(&thread, "sectionEnteredAt").is_null() {
            field(&thread, "sectionEnteredAt").clone()
        } else {
            self.now()
        };
        self.transaction(||{
            if old!=section&&old.is_some() {self.renumber_section(&old_ids)?;}
            if section.is_some() {self.renumber_section(&target_ids)?;}
            self.run("UPDATE threads SET section_id = ?, section_entered_at = ?, section_position = ?, is_pinned = ? WHERE id = ?",&[section.cloned().map(IvyJson::String).unwrap_or(IvyJson::Null),entered,position.map(|p|IvyJson::Number(p as f64)).unwrap_or(IvyJson::Null),IvyJson::Number(if section==Some(&PINNED_SECTION_ID.into()) {1.0} else {0.0}),IvyJson::String(id.clone())])?;Ok(())
        })?;
        self.get_thread(id)?.ok_or_else(unknown)
    }
    pub fn update_thread_status(&self, id: &JsString, status: &IvyJson) -> Result<()> {
        self.run(
            "UPDATE threads SET status_json = ?, updated_at = ? WHERE id = ?",
            &[
                encode(status)?.into(),
                self.now(),
                IvyJson::String(id.clone()),
            ],
        )?;
        Ok(())
    }
    fn update_thread_field(&self, column: &str, id: &JsString, value: &IvyJson) -> Result<()> {
        self.run(
            &format!("UPDATE threads SET {column} = ?, updated_at = ? WHERE id = ?"),
            &[value.clone(), self.now(), IvyJson::String(id.clone())],
        )?;
        Ok(())
    }
    pub fn update_thread_name(&self, id: &JsString, name: Option<&JsString>) -> Result<()> {
        self.update_thread_field(
            "name",
            id,
            &name.cloned().map(IvyJson::String).unwrap_or(IvyJson::Null),
        )
    }
    pub fn update_claude_session_id(
        &self,
        id: &JsString,
        session: Option<&JsString>,
    ) -> Result<()> {
        self.update_thread_field(
            "claude_session_id",
            id,
            &session
                .cloned()
                .map(IvyJson::String)
                .unwrap_or(IvyJson::Null),
        )
    }
    pub fn update_codex_session_id(&self, id: &JsString, session: Option<&JsString>) -> Result<()> {
        self.update_thread_field(
            "codex_session_id",
            id,
            &session
                .cloned()
                .map(IvyJson::String)
                .unwrap_or(IvyJson::Null),
        )
    }
    pub fn set_archived(&self, id: &JsString, archived: bool) -> Result<()> {
        self.update_thread_field(
            "archived",
            id,
            &IvyJson::Number(if archived { 1.0 } else { 0.0 }),
        )
    }
    pub fn upsert_turn(&self, turn: &IvyJson) -> Result<()> {
        require_bindings(
            turn,
            &[
                ("id", 1),
                ("threadId", 2),
                ("status", 3),
                ("startedAt", 4),
                ("completedAt", 5),
                ("durationMs", 6),
                ("items", 7),
                ("diff", 8),
            ],
        )?;
        let mut values = [
            "id",
            "threadId",
            "status",
            "startedAt",
            "completedAt",
            "durationMs",
        ]
        .iter()
        .map(|key| field(turn, key).clone())
        .collect::<Vec<_>>();
        values.extend([
            encode(field(turn, "items"))?.into(),
            field(turn, "diff").clone(),
            if field(turn, "error").is_null() {
                IvyJson::Null
            } else {
                encode(field(turn, "error"))?.into()
            },
        ]);
        self.run("INSERT INTO turns (id, thread_id, status, started_at, completed_at, duration_ms, items_json, diff, error_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET status=excluded.status, started_at=excluded.started_at, completed_at=excluded.completed_at, duration_ms=excluded.duration_ms, items_json=excluded.items_json, diff=excluded.diff, error_json=excluded.error_json",&values)?;
        Ok(())
    }
    pub fn get_turn(&self, id: &JsString) -> Result<Option<IvyJson>> {
        self.one(
            "SELECT * FROM turns WHERE id = ?",
            &[IvyJson::String(id.clone())],
        )?
        .map(|row| row_to_turn(&row))
        .transpose()
    }
    pub fn list_turns(&self, thread: &JsString) -> Result<Vec<IvyJson>> {
        self.all(
            "SELECT * FROM turns WHERE thread_id = ? ORDER BY started_at ASC",
            &[IvyJson::String(thread.clone())],
        )?
        .iter()
        .map(row_to_turn)
        .collect()
    }
    pub fn delete_recent_turns(&self, thread: &JsString, count: f64) -> Result<usize> {
        if count <= 0.0 {
            return Ok(0);
        }
        let rows = self.all(
            "SELECT id FROM turns WHERE thread_id = ? ORDER BY started_at DESC LIMIT ?",
            &[IvyJson::String(thread.clone()), IvyJson::Number(count)],
        )?;
        for row in &rows {
            self.run(
                "DELETE FROM turns WHERE id = ?",
                &[field(row, "id").clone()],
            )?;
        }
        Ok(rows.len())
    }
    pub fn append_item(&self, id: &JsString, item: &IvyJson) -> Result<Option<IvyJson>> {
        let Some(mut turn) = self.get_turn(id)? else {
            return Ok(None);
        };
        let mut items = item_array(&turn, "push")?.to_vec();
        items.push(decode(&encode(item)?.into())?);
        turn.insert("items", IvyJson::Array(items));
        self.upsert_turn(&turn)?;
        Ok(Some(turn))
    }
    pub fn update_item(
        &self,
        id: &JsString,
        item_id: &JsString,
        mut updater: impl FnMut(IvyJson) -> Result<IvyJson>,
    ) -> Result<Option<IvyJson>> {
        let Some(mut turn) = self.get_turn(id)? else {
            return Ok(None);
        };
        let items = item_array(&turn, "map")?;
        let mut next = Vec::with_capacity(items.len());
        for item in items {
            if item.is_null() {
                return Err(StoreError {
                    name: "TypeError",
                    message: "Cannot read properties of null (reading 'id')".into(),
                    code: None,
                });
            }
            next.push(if field(item, "id").as_js_string() == Some(item_id) {
                updater(item.clone())?
            } else {
                item.clone()
            });
        }
        turn.insert("items", IvyJson::Array(next));
        self.upsert_turn(&turn)?;
        Ok(Some(turn))
    }
    pub fn update_item_and_move_to_end(
        &self,
        id: &JsString,
        item_id: &JsString,
        updater: impl FnOnce(IvyJson) -> Result<IvyJson>,
    ) -> Result<Option<IvyJson>> {
        let Some(mut turn) = self.get_turn(id)? else {
            return Ok(None);
        };
        let items = item_array(&turn, "findIndex")?;
        let mut index = None;
        for (i, item) in items.iter().enumerate() {
            if item.is_null() {
                return Err(StoreError {
                    name: "TypeError",
                    message: "Cannot read properties of null (reading 'id')".into(),
                    code: None,
                });
            }
            if field(item, "id").as_js_string() == Some(item_id) {
                index = Some(i);
                break;
            }
        }
        let Some(index) = index else {
            return Ok(Some(turn));
        };
        let mut next = items.to_vec();
        let current = next.remove(index);
        let updated = updater(current)?;
        next.push(updated);
        turn.insert("items", IvyJson::Array(next));
        self.upsert_turn(&turn)?;
        Ok(Some(turn))
    }
    pub fn complete_turn(
        &self,
        id: &JsString,
        status: &JsString,
        error: &IvyJson,
    ) -> Result<Option<IvyJson>> {
        let Some(mut turn) = self.get_turn(id)? else {
            return Ok(None);
        };
        let now = self.now();
        let duration = if field(&turn, "startedAt").is_null() {
            IvyJson::Null
        } else {
            IvyJson::Number(nonnegative(
                (number(&now) - number(field(&turn, "startedAt"))) * 1000.0,
            ))
        };
        turn.insert("status", IvyJson::String(status.clone()));
        turn.insert("completedAt", now);
        turn.insert("durationMs", duration);
        turn.insert("error", error.clone());
        self.upsert_turn(&turn)?;
        Ok(Some(turn))
    }
    pub fn update_turn_diff(&self, id: &JsString, diff: &JsString) -> Result<Option<IvyJson>> {
        let Some(mut turn) = self.get_turn(id)? else {
            return Ok(None);
        };
        turn.insert("diff", IvyJson::String(diff.clone()));
        self.upsert_turn(&turn)?;
        Ok(Some(turn))
    }
    pub fn recover_stale_in_progress_turns(&self, message: &JsString) -> Result<usize> {
        let rows = self.all(
            "SELECT id, thread_id, started_at, items_json FROM turns WHERE status = ?",
            &["inProgress".into()],
        )?;
        let now = self.now();
        let error = encode(&IvyJson::object([(
            "message",
            IvyJson::String(message.clone()),
        )]))?;
        let mut seen = Vec::new();
        let mut count = 0;
        for row in &rows {
            let duration = if field(row, "started_at").is_null() {
                IvyJson::Null
            } else {
                IvyJson::Number(nonnegative(
                    (number(&now) - number(field(row, "started_at"))) * 1000.0,
                ))
            };
            self.run("UPDATE turns SET status = ?, completed_at = ?, duration_ms = ?, error_json = ?, items_json = ? WHERE id = ?",&["interrupted".into(),now.clone(),duration,error.clone().into(),terminalize_items(&text(field(row,"items_json")),message,true,false)?.into(),field(row,"id").clone()])?;
            let thread = js_string(field(row, "thread_id"));
            if !seen.contains(&thread) {
                seen.push(thread);
            }
            count += 1;
        }
        let idle = encode(&IvyJson::object([("type", "idle".into())]))?;
        for thread in seen {
            self.run(
                "UPDATE threads SET status_json = ?, updated_at = ? WHERE id = ?",
                &[idle.clone().into(), now.clone(), IvyJson::String(thread)],
            )?;
        }
        let rows = self.all(
            "SELECT id, thread_id, status, items_json FROM turns WHERE status <> ?",
            &["inProgress".into()],
        )?;
        let mut repairs = Vec::new();
        for row in rows {
            let completed = is(field(&row, "status"), "completed");
            let old = text(field(&row, "items_json"));
            let next = terminalize_items(&old, message, !completed, completed)?;
            if next == old {
                continue;
            }
            self.run(
                "UPDATE turns SET items_json = ? WHERE id = ?",
                &[next.into(), field(&row, "id").clone()],
            )?;
            let thread = js_string(field(&row, "thread_id"));
            if !repairs.contains(&thread) {
                repairs.push(thread);
            }
            count += 1;
        }
        for thread in repairs {
            self.run("UPDATE threads SET status_json = ?, updated_at = ? WHERE id = ? AND NOT EXISTS (SELECT 1 FROM turns WHERE thread_id = threads.id AND status = ?)",&[idle.clone().into(),now.clone(),IvyJson::String(thread),"inProgress".into()])?;
        }
        count+=self.run("UPDATE threads SET status_json = ?, updated_at = ? WHERE json_extract(status_json, '$.type') = 'active' AND NOT EXISTS (SELECT 1 FROM turns WHERE thread_id = threads.id AND status = ?)",&[idle.into(),now,"inProgress".into()])?;
        Ok(count)
    }
    pub fn close(self) -> Result<()> {
        self.db.close().map_err(|(_, e)| e.into())
    }
}

fn row_to_thread(row: &IvyJson) -> Result<IvyJson> {
    let mut result = IvyJson::object([(
        "collaborationMode",
        if is(field(row, "collaboration_mode"), "plan") {
            "plan"
        } else {
            "default"
        }
        .into(),
    )]);
    for (key, column) in [
        ("id", "id"),
        ("sessionId", "session_id"),
        ("forkedFromId", "forked_from_id"),
    ] {
        result.insert(
            key,
            if key == "forkedFromId" {
                nullable_text(field(row, column))
            } else {
                IvyJson::String(js_string(field(row, column)))
            },
        );
    }
    result.insert(
        "isPinned",
        (is(field(row, "section_id"), PINNED_SECTION_ID) || number(field(row, "is_pinned")) == 1.0)
            .into(),
    );
    result.insert("sectionId", nullable_text(field(row, "section_id")));
    result.insert(
        "sectionEnteredAt",
        nullable_number(field(row, "section_entered_at")),
    );
    result.insert(
        "sectionPosition",
        nullable_number(field(row, "section_position")),
    );
    result.insert(
        "preview",
        IvyJson::String(js_string(&coalesce(field(row, "preview"), &"".into()))),
    );
    result.insert("name", nullable_text(field(row, "name")));
    result.insert("archived", (number(field(row, "archived")) == 1.0).into());
    for (key, column) in [("cwd", "cwd"), ("model", "model")] {
        result.insert(key, IvyJson::String(js_string(field(row, column))));
    }
    result.insert(
        "reasoningEffort",
        nullable_text(field(row, "reasoning_effort")),
    );
    result.insert(
        "modelProvider",
        IvyJson::String(js_string(field(row, "model_provider"))),
    );
    result.insert(
        "claudeSessionId",
        nullable_text(field(row, "claude_session_id")),
    );
    result.insert("source", IvyJson::String(js_string(field(row, "source"))));
    result.insert(
        "createdAt",
        IvyJson::Number(number(field(row, "created_at"))),
    );
    result.insert(
        "updatedAt",
        IvyJson::Number(number(field(row, "updated_at"))),
    );
    result.insert("status", decode(field(row, "status_json"))?);
    for (key, column) in [
        ("approvalPolicy", "approval_policy"),
        ("sandboxMode", "sandbox_mode"),
        ("permissionProfileId", "permission_profile_id"),
    ] {
        result.insert(key, nullable_text(field(row, column)));
    }
    result.insert("ephemeral", (number(field(row, "ephemeral")) == 1.0).into());
    for (key, column) in [
        ("threadSource", "thread_source"),
        ("agentRole", "agent_role"),
        ("agentNickname", "agent_nickname"),
        ("baseInstructions", "base_instructions"),
        ("developerInstructions", "developer_instructions"),
        ("personality", "personality"),
    ] {
        result.insert(key, nullable_text(field(row, column)));
    }
    result.insert(
        "runtimeBackend",
        if is(field(row, "runtime_backend"), "codex") {
            "codex"
        } else {
            "claude"
        }
        .into(),
    );
    result.insert(
        "codexSessionId",
        nullable_text(field(row, "codex_session_id")),
    );
    Ok(result)
}
fn row_to_section(row: &IvyJson) -> Result<IvyJson> {
    let appearance = if field(row, "appearance_json").is_null() {
        None
    } else {
        decode(field(row, "appearance_json")).ok()
    };
    let appearance = appearance
        .filter(|a| a.as_object().is_some())
        .map(|a| {
            IvyJson::object([
                (
                    "color",
                    if field(&a, "color").as_js_string().is_some() {
                        field(&a, "color").clone()
                    } else {
                        IvyJson::Null
                    },
                ),
                (
                    "icon",
                    if field(&a, "icon").as_js_string().is_some() {
                        field(&a, "icon").clone()
                    } else {
                        IvyJson::Null
                    },
                ),
            ])
        })
        .unwrap_or(IvyJson::Null);
    Ok(IvyJson::object([
        ("id", IvyJson::String(js_string(field(row, "id")))),
        (
            "name",
            IvyJson::String(js_string(&coalesce(field(row, "name"), &"".into()))),
        ),
        ("appearance", appearance),
    ]))
}
fn row_to_turn(row: &IvyJson) -> Result<IvyJson> {
    Ok(IvyJson::object([
        ("id", IvyJson::String(js_string(field(row, "id")))),
        (
            "threadId",
            IvyJson::String(js_string(field(row, "thread_id"))),
        ),
        ("status", IvyJson::String(js_string(field(row, "status")))),
        ("startedAt", nullable_number(field(row, "started_at"))),
        ("completedAt", nullable_number(field(row, "completed_at"))),
        ("durationMs", nullable_number(field(row, "duration_ms"))),
        ("items", decode(field(row, "items_json"))?),
        (
            "diff",
            IvyJson::String(js_string(&coalesce(field(row, "diff"), &"".into()))),
        ),
        (
            "error",
            if field(row, "error_json").is_null() {
                IvyJson::Null
            } else {
                decode(field(row, "error_json"))?
            },
        ),
    ]))
}
fn terminalize_items(
    source: &str,
    message: &JsString,
    terminalize_activity: bool,
    strip_completed: bool,
) -> Result<String> {
    let Some(parsed) = parse(source, usize::MAX).ok().and_then(IvyJson::into_array) else {
        return Ok("[]".into());
    };
    let mut items = Vec::new();
    for mut item in parsed {
        if item.is_null() {
            continue;
        }
        if item.as_object().is_none() {
            items.push(item);
            continue;
        }
        if is(field(&item, "type"), "subAgentActivity") {
            if strip_completed {
                continue;
            }
            if is(field(&item, "kind"), "completed")
                || (terminalize_activity
                    && (is(field(&item, "kind"), "started")
                        || is(field(&item, "kind"), "interacted")))
            {
                item.insert("kind", "interrupted".into());
                items.push(item);
                continue;
            }
        }
        if is(field(&item, "type"), "collabAgentToolCall") {
            let states = field(&item, "agentsStates");
            let next = if let Some(states) = states.as_object() {
                Some(IvyJson::object(states.iter().map(|(id, state)| {
                    let mut next = state.clone();
                    if next.as_object().is_some()
                        && (is(field(&next, "status"), "pendingInit")
                            || is(field(&next, "status"), "running"))
                    {
                        next.insert("status", "errored".into());
                        next.insert(
                            "message",
                            coalesce(field(&next, "message"), &IvyJson::String(message.clone())),
                        );
                    }
                    (id.clone(), next)
                })))
            } else {
                item.get("agentsStates").cloned()
            };
            if is(field(&item, "status"), "inProgress") {
                item.insert("status", "failed".into());
            }
            if let Some(next) = next {
                item.insert("agentsStates", next);
            }
        } else if is(field(&item, "status"), "inProgress") {
            item.insert("status", "failed".into());
        }
        items.push(item);
    }
    encode(&IvyJson::Array(items))
}
