//! Native state_5 working-copy relocation. Never open a source or backup here.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, hash::hash_json, json, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_store::rusqlite::{
    Connection,
    config::DbConfig,
    types::{Value, ValueRef},
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

type Replacements = BTreeMap<String, BTreeMap<JsString, BTreeMap<JsString, JsString>>>;
const ROW_LIMIT: usize = 1_000_000;

fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn sqlite<T>(result: ivy_store::rusqlite::Result<T>) -> IvyResult<T> {
    result.map_err(|_| IvyError::internal())
}
fn sql_name(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
fn text(bytes: &[u8]) -> JsString {
    String::from_utf8_lossy(bytes).into_owned().into()
}
fn hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len() * 2);
    const DIGITS: &[u8] = b"0123456789abcdef";
    for byte in bytes {
        value.push(DIGITS[(byte >> 4) as usize] as char);
        value.push(DIGITS[(byte & 15) as usize] as char);
    }
    value
}

/// DatabaseSync's ordinary reads reject integers that cannot be represented
/// exactly as a JavaScript Number. Digest reads deliberately use BigInts instead.
fn ordinary(value: ValueRef<'_>) -> IvyResult<IvyJson> {
    Ok(match value {
        ValueRef::Null => IvyJson::Null,
        ValueRef::Integer(value) => {
            if !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&value) {
                return Err(IvyError::internal());
            }
            IvyJson::Number(value as f64)
        }
        ValueRef::Real(value) => IvyJson::Number(value),
        ValueRef::Text(value) => IvyJson::String(text(value)),
        // Schema fields are scalar. Migration blobs and path values retain
        // their SQLite type at their respective validation boundaries below.
        ValueRef::Blob(_) => return Err(IvyError::internal()),
    })
}

fn migration_value(value: ValueRef<'_>) -> IvyResult<(IvyJson, bool)> {
    // A SQLite blob is a Uint8Array in the original. It fails JSON validation
    // at its value's position. NaN reserves the same failing node so the shared
    // canonical encoder still owns byte/node limits and traversal order.
    match value {
        ValueRef::Blob(_) => Ok((IvyJson::Number(f64::NAN), true)),
        value => Ok((ordinary(value)?, false)),
    }
}

pub fn verify_layout(db: &Connection, layout: &IvyJson) -> IvyResult<()> {
    let version: i64 = sqlite(db.query_row("PRAGMA user_version", [], |row| row.get(0)))?;
    require(
        version == 0,
        "unsupported_storage",
        "Native storage user version is unsupported.",
    )?;
    let mut statement = sqlite(db.prepare("SELECT type,name,tbl_name AS tableName,sql FROM sqlite_master WHERE sql IS NOT NULL AND name <> '_sqlx_migrations' ORDER BY type,name"))?;
    let mut rows = sqlite(statement.query([]))?;
    let mut schema = Vec::new();
    while let Some(row) = sqlite(rows.next())? {
        let mut value = IvyJson::object([
            ("type", ordinary(sqlite(row.get_ref(0))?)?),
            ("name", ordinary(sqlite(row.get_ref(1))?)?),
            ("tableName", ordinary(sqlite(row.get_ref(2))?)?),
            ("sql", ordinary(sqlite(row.get_ref(3))?)?),
        ]);
        let sql = value.required("sql")?.as_str()?.replace("\r\n", "\n");
        value.insert("sql", sql.into());
        schema.push(value);
    }
    require(
        hash_json(&IvyJson::Array(schema))? == hash_json(layout.required("schema")?)?,
        "unsupported_storage",
        "Native storage schema differs from its pinned public migrations.",
    )?;
    let mut statement = sqlite(db.prepare("SELECT version,description,lower(hex(checksum)) AS checksum,success FROM _sqlx_migrations ORDER BY version"))?;
    let mut rows = sqlite(statement.query([]))?;
    let mut migrations = Vec::new();
    let mut complete = true;
    let mut first_invalid_is_blob = None;
    while let Some(row) = sqlite(rows.next())? {
        let (version, version_blob) = migration_value(sqlite(row.get_ref(0))?)?;
        let (description, description_blob) = migration_value(sqlite(row.get_ref(1))?)?;
        let checksum = ordinary(sqlite(row.get_ref(2))?)?;
        let success = sqlite(row.get_ref(3))?;
        complete &=
            !matches!(success, ValueRef::Blob(_)) && ordinary(success)? == IvyJson::Number(1.0);
        // Canonical keys are checksum, description, version; SQLite fetch
        // failures must still precede this deferred JSON-domain rejection.
        for (value, blob) in [
            (&checksum, false),
            (&description, description_blob),
            (&version, version_blob),
        ] {
            if first_invalid_is_blob.is_none()
                && matches!(value, IvyJson::Number(number)
                if !number.is_finite() || (number.fract()==0.0 && number.abs()>9_007_199_254_740_991.0))
            {
                first_invalid_is_blob = Some(blob);
            }
        }
        migrations.push(IvyJson::object([
            ("version", version),
            ("description", description),
            ("checksum", checksum),
        ]));
    }
    require(
        complete,
        "unsupported_storage",
        "Native migration checksums or completion differ from the pinned build.",
    )?;
    let observed = hash_json(&IvyJson::Array(migrations)).map_err(|error| {
        if first_invalid_is_blob == Some(true)
            && error.code == "invalid_arguments"
            && error.message == "Numbers must be finite and integers must be safe."
        {
            IvyError::new("invalid_arguments", "JSON objects must be plain objects.")
        } else {
            error
        }
    })?;
    require(
        observed == hash_json(layout.required("migrations")?)?,
        "unsupported_storage",
        "Native migration checksums or completion differ from the pinned build.",
    )
}

pub fn verify_integrity(db: &Connection) -> IvyResult<()> {
    let mut statement = sqlite(db.prepare("PRAGMA quick_check"))?;
    let values = sqlite(statement.query_map([], |row| row.get::<_, String>(0)))?
        .collect::<ivy_store::rusqlite::Result<Vec<_>>>();
    let intact = sqlite(values)?.iter().all(|value| value == "ok");
    let foreign_keys = if intact {
        let mut statement = sqlite(db.prepare("PRAGMA foreign_key_check"))?;
        let mut rows = sqlite(statement.query([]))?;
        let mut count = 0;
        while sqlite(rows.next())?.is_some() {
            count += 1;
        }
        count == 0
    } else {
        false
    };
    require(
        intact && foreign_keys,
        "backup_invalid",
        "Native recovery requires intact storage and foreign keys.",
    )
}

fn digest(db: &Connection, inverse: &Replacements) -> IvyResult<String> {
    let mut hash = Sha256::new();
    let mut statement =
        sqlite(db.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"))?;
    let tables = sqlite(statement.query_map([], |row| row.get::<_, String>(0)))?
        .collect::<ivy_store::rusqlite::Result<Vec<_>>>();
    for table in sqlite(tables)? {
        hash.update(json::canonical(
            &table.clone().into(),
            MANAGEMENT_FRAME_BYTES,
        )?);
        hash.update(b"\n");
        let mut statement = sqlite(db.prepare(&format!(
            "SELECT * FROM {} ORDER BY rowid",
            sql_name(&table)
        )))?;
        let names: Vec<JsString> = statement
            .column_names()
            .iter()
            .map(|name| (*name).into())
            .collect();
        let mut rows = sqlite(statement.query([]))?;
        let mut count = 0;
        while let Some(row) = sqlite(rows.next())? {
            count += 1;
            require(
                count <= ROW_LIMIT,
                "limit_exceeded",
                "Native recovery table exceeds its bounded row workload.",
            )?;
            let mut object = IvyJson::object([] as [(&str, IvyJson); 0]);
            for (index, name) in names.iter().enumerate() {
                let (kind, value) = match sqlite(row.get_ref(index))? {
                    ValueRef::Null => ("null", IvyJson::Null),
                    ValueRef::Integer(value) => ("bigint", value.to_string().into()),
                    ValueRef::Real(value) => ("number", IvyJson::Number(value)),
                    ValueRef::Blob(value) => ("blob", hex(value).into()),
                    ValueRef::Text(value) => {
                        let before = text(value);
                        let value = inverse
                            .get(&table)
                            .and_then(|table| table.get(name))
                            .and_then(|column| column.get(&before))
                            .unwrap_or(&before)
                            .clone();
                        ("string", IvyJson::String(value))
                    }
                };
                object.insert(
                    name.clone(),
                    IvyJson::Array(vec![IvyJson::String(name.clone()), kind.into(), value]),
                );
            }
            // Object.entries enumerates integer-index column names first.
            let values = IvyJson::Array(
                json::object_entries(&object)
                    .into_iter()
                    .map(|(_, value)| value.clone())
                    .collect(),
            );
            hash.update(json::canonical(&values, MANAGEMENT_FRAME_BYTES)?);
            hash.update(b"\n");
        }
    }
    Ok(format!("sha256:{}", hex(&hash.finalize())))
}

pub fn logical_digest(db: &Connection) -> IvyResult<String> {
    digest(db, &Replacements::new())
}

#[derive(Debug, Clone)]
pub struct RelocatedDatabase {
    pub logical_data_hash: String,
    pub changes: Vec<IvyJson>,
}

/// Consumes the connection to a fresh, activation-held working copy. The caller
/// validates the retained layout and supplies the file-verifying pointer mapper.
/// This preserves the original commit-before-checkpoint failure boundary.
pub fn relocate_database(
    db: Connection,
    layout: &IvyJson,
    mut mapped_file: impl FnMut(&JsString, &str) -> IvyResult<Option<JsString>>,
) -> IvyResult<RelocatedDatabase> {
    let result = (|| {
        sqlite(db.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DML, false))?;
        sqlite(db.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DDL, false))?;
        sqlite(db.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=0; PRAGMA synchronous=FULL; BEGIN IMMEDIATE"))?;
        verify_layout(&db, layout)?;
        verify_integrity(&db)?;
        let logical_data_hash = logical_digest(&db)?;
        let mut inverse = Replacements::new();
        let mut changes = Vec::new();
        for definition in layout
            .required("pathColumns")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            let table = definition.required("table")?.as_str()?;
            let column = definition
                .required("column")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?
                .clone();
            let scope = definition.required("scope")?.as_str()?;
            let column_sql = sql_name(&column.to_utf8()?);
            let table_sql = sql_name(&table);
            let mut statement = sqlite(db.prepare(&format!("SELECT DISTINCT {column_sql} AS path FROM {table_sql} WHERE {column_sql} IS NOT NULL LIMIT 1000001")))?;
            let mut rows = sqlite(statement.query([]))?;
            let mut paths = Vec::new();
            while let Some(row) = sqlite(rows.next())? {
                let value = sqlite(row.get_ref(0))?;
                if let ValueRef::Integer(_) = value {
                    ordinary(value)?;
                }
                paths.push(match value {
                    ValueRef::Text(bytes) => Some(text(bytes)),
                    _ => None,
                });
            }
            drop(rows);
            drop(statement);
            require(
                paths.len() <= ROW_LIMIT,
                "limit_exceeded",
                "Native recovery has too many distinct pointers.",
            )?;
            let mut mapping = BTreeMap::new();
            let mut updates = Vec::new();
            for before in &paths {
                let before = before.as_ref().ok_or_else(|| {
                    IvyError::new("backup_invalid", "Native file pointer is not text.")
                })?;
                let Some(after) = mapped_file(before, &scope)? else {
                    continue;
                };
                require(
                    !mapping.contains_key(&after),
                    "backup_invalid",
                    "Distinct native pointers collide in the restored home.",
                )?;
                mapping.insert(after.clone(), before.clone());
                updates.push((after, before.clone()));
            }
            let original_values: BTreeSet<_> = paths.iter().filter_map(Option::as_ref).collect();
            require(
                mapping.keys().all(|after| !original_values.contains(after)),
                "backup_invalid",
                "Native relocation collides with an existing pointer.",
            )?;
            let mut count = 0u64;
            let mut update = sqlite(db.prepare(&format!(
                "UPDATE {table_sql} SET {column_sql}=? WHERE {column_sql}=?"
            )))?;
            for (after, before) in updates {
                let values = [
                    Value::Text(String::from_utf16_lossy(&after.0)),
                    Value::Text(String::from_utf16_lossy(&before.0)),
                ];
                count +=
                    sqlite(update.execute(ivy_store::rusqlite::params_from_iter(values)))? as u64;
            }
            drop(update);
            inverse
                .entry(table.clone())
                .or_default()
                .insert(column.clone(), mapping);
            changes.push(IvyJson::object([
                ("table", table.into()),
                ("column", IvyJson::String(column)),
                ("rows", IvyJson::Number(count as f64)),
            ]));
        }
        require(
            digest(&db, &inverse)? == logical_data_hash,
            "backup_changed",
            "Native relocation changed data outside its declared pointers.",
        )?;
        verify_integrity(&db)?;
        sqlite(db.execute_batch("COMMIT"))?;
        let busy: i64 =
            sqlite(db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0)))?;
        require(
            busy == 0,
            "backup_busy",
            "Native working index checkpoint was not exclusive.",
        )?;
        Ok(RelocatedDatabase {
            logical_data_hash,
            changes,
        })
    })();
    if !db.is_autocommit() {
        sqlite(db.execute_batch("ROLLBACK"))?;
    }
    db.close().map_err(|_| IvyError::internal())?;
    result
}
