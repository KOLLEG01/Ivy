//! Read-only format-4 inspection and offline snapshotting. The host caller must
//! establish stopped-writer ownership before using either filesystem helper.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    core_schema::{validate_host, validate_shared},
    json::stringify,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, backup::Backup, params};
use std::{collections::BTreeSet, fs::OpenOptions, path::Path, time::Duration};

pub const STORAGE_FORMAT: i64 = 4;
fn internal<T>(_: T) -> IvyError {
    IvyError::internal()
}
fn retained_path_exists(path: &Path) -> IvyResult<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(internal(error)),
    }
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn object(pairs: impl IntoIterator<Item = (&'static str, IvyJson)>) -> IvyJson {
    IvyJson::Object(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
}
fn sqlite_text(value: &IvyJson) -> IvyResult<String> {
    value
        .as_js_string()
        .map(|s| String::from_utf16_lossy(&s.0))
        .ok_or_else(IvyError::internal)
}
fn read_connection(path: &Path) -> IvyResult<Connection> {
    let db =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(internal)?;
    db.busy_timeout(Duration::from_secs(1)).map_err(internal)?;
    db.execute_batch("PRAGMA query_only=ON; BEGIN")
        .map_err(internal)?;
    Ok(db)
}
fn format(db: &Connection) -> IvyResult<i64> {
    db.query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(internal)
}

/// Includes every retained historical version by default, including archived
/// objects. Explicit evidence pins extend a narrower current/subtree read scope.
pub fn contract_versions(
    db: &Connection,
    key: &JsString,
    scope: Option<&IvyJson>,
) -> IvyResult<Vec<String>> {
    let default = object([
        ("roots", IvyJson::Null),
        ("history", "all".into()),
        ("includeArchived", true.into()),
        ("references", IvyJson::Array(vec![])),
    ]);
    let scope = scope
        .filter(|v| !matches!(v, IvyJson::Null))
        .unwrap_or(&default);
    validate_shared("ContractReadScope", scope)?;
    let roots = scope.required("roots")?;
    let references = scope
        .required("references")?
        .as_array()
        .ok_or_else(IvyError::internal)?;
    let key = String::from_utf16_lossy(&key.0);
    if let Some(roots) = roots.as_array() {
        let mut exists = db
            .prepare("SELECT 1 FROM objects WHERE id=?")
            .map_err(internal)?;
        for root in roots {
            require(
                exists
                    .query_row([sqlite_text(root)?], |row| row.get::<_, i64>(0))
                    .optional()
                    .map_err(internal)?
                    .is_some(),
                "not_found",
                "A contract read root does not exist.",
            )?;
        }
    }
    let mut exists = db
        .prepare("SELECT 1 FROM revisions WHERE object_id=? AND revision=? AND contract_key=?")
        .map_err(internal)?;
    for pin in references {
        require(
            exists
                .query_row(
                    params![
                        sqlite_text(pin.required("objectId")?)?,
                        pin.required("revision")?
                            .as_f64()
                            .ok_or_else(IvyError::internal)? as i64,
                        key
                    ],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(internal)?
                .is_some(),
            "not_found",
            "A required contract evidence revision does not exist.",
        )?;
    }
    let all = scope.get("history") == Some(&IvyJson::from("all"));
    let archived = scope.get("includeArchived") == Some(&IvyJson::from(true));
    let versions = if matches!(roots, IvyJson::Null) && all && archived {
        db.prepare("SELECT DISTINCT contract_version FROM revisions WHERE contract_key=? ORDER BY contract_version LIMIT 65").map_err(internal)?.query_map([&key],|row|row.get::<_,String>(0)).map_err(internal)?.collect::<Result<Vec<_>,_>>().map_err(internal)?
    } else {
        let empty = IvyJson::Array(vec![]);
        let roots_json = stringify(
            if matches!(roots, IvyJson::Null) {
                &empty
            } else {
                roots
            },
            32 * 1024 * 1024,
        )?;
        let references_json = stringify(scope.required("references")?, 32 * 1024 * 1024)?;
        db.prepare("WITH RECURSIVE selected(id) AS (
          SELECT value FROM json_each(?) UNION SELECT o.id FROM objects o JOIN selected s ON o.parent_id=s.id
        ) SELECT DISTINCT r.contract_version FROM revisions r JOIN objects o ON o.id=r.object_id
        WHERE r.contract_key=? AND (
          ((? OR o.id IN (SELECT id FROM selected)) AND (? OR o.effective_archive=0) AND (? OR r.revision=o.current_revision))
          OR EXISTS (SELECT 1 FROM json_each(?) p WHERE json_extract(p.value,'$.objectId')=r.object_id AND json_extract(p.value,'$.revision')=r.revision)
        ) ORDER BY r.contract_version LIMIT 65").map_err(internal)?
          .query_map(params![roots_json,key,matches!(roots,IvyJson::Null),archived,all,references_json],|row|row.get::<_,String>(0)).map_err(internal)?.collect::<Result<Vec<_>,_>>().map_err(internal)?
    };
    require(
        versions.len() <= 64,
        "limit_exceeded",
        "A requested read domain has too many contract versions.",
    )?;
    Ok(versions)
}

fn inspection(exists: bool, contracts: Vec<IvyJson>) -> IvyJson {
    object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("exists", exists.into()),
        ("format", IvyJson::Number(if exists { 4.0 } else { 0.0 })),
        ("contracts", IvyJson::Array(contracts)),
    ])
}

pub fn storage_inspection(db: &Connection, requirements: &[IvyJson]) -> IvyResult<IvyJson> {
    // JavaScript maps value.key before applying the bounded-key assertion.
    if requirements
        .iter()
        .any(|value| matches!(value, IvyJson::Null))
    {
        return Err(IvyError::internal());
    }
    let mut keys = BTreeSet::new();
    require(
        requirements.len() <= 128
            && requirements.iter().all(|r| {
                r.get("key")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|key| {
                        !key.0.is_empty() && key.0.len() <= 192 && keys.insert(key.clone())
                    })
            }),
        "invalid_arguments",
        "Storage inspection needs bounded distinct contract keys.",
    )?;
    require(
        format(db)? == STORAGE_FORMAT,
        "unsupported_storage_format",
        "This Hive inspector does not understand the stored format.",
    )?;
    let contracts = requirements
        .iter()
        .map(|required| {
            let key = required
                .required("key")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?;
            Ok(object([
                ("key", IvyJson::String(key.clone())),
                (
                    "versions",
                    IvyJson::Array(
                        contract_versions(db, key, required.get("readScope"))?
                            .into_iter()
                            .map(IvyJson::from)
                            .collect(),
                    ),
                ),
            ]))
        })
        .collect::<IvyResult<Vec<_>>>()?;
    let result = inspection(true, contracts);
    validate_host("StorageInspection", &result)?;
    Ok(result)
}

pub fn inspect_stopped_storage(path: &Path, requirements: &[IvyJson]) -> IvyResult<IvyJson> {
    if !path.exists() {
        if requirements
            .iter()
            .any(|value| matches!(value, IvyJson::Null))
        {
            return Err(IvyError::internal());
        }
        // Match the missing-file branch without opening or creating a database.
        let contracts = requirements
            .iter()
            .map(|r| {
                let mut fields = vec![];
                if let Some(key) = r.get("key") {
                    fields.push(("key", key.clone()));
                }
                fields.push(("versions", IvyJson::Array(vec![])));
                object(fields)
            })
            .collect();
        return Ok(inspection(false, contracts));
    }
    let db = read_connection(path)?;
    storage_inspection(&db, requirements)
}

pub fn snapshot_stopped_hive(source: &Path, destination: &Path) -> IvyResult<IvyJson> {
    const CONFLICT: &str =
        "Offline Hive snapshot needs an existing source and a fresh destination.";
    require(
        source.has_root()
            && destination.has_root()
            && retained_path_exists(source)?
            && !retained_path_exists(destination)?,
        "target_conflict",
        CONFLICT,
    )?;
    let db = read_connection(source)?;
    require(
        format(&db)? == STORAGE_FORMAT,
        "unsupported_storage_format",
        "This Hive backup helper cannot read the stored format.",
    )?;
    let valid = db
        .prepare("PRAGMA quick_check")
        .map_err(internal)?
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(internal)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(internal)?
        .iter()
        .all(|v| v == "ok");
    require(
        valid,
        "storage_invalid",
        "Hive snapshot refused an invalid database.",
    )?;
    // Reserve the fresh path exclusively so a concurrent file cannot be overwritten.
    let mut options = OpenOptions::new();
    options.write(true).read(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(destination).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            IvyError::new("target_conflict", CONFLICT)
        } else {
            internal(error)
        }
    })?;
    let mut target = Connection::open(destination).map_err(internal)?;
    let backup = Backup::new(&db, &mut target).map_err(internal)?;
    backup
        .run_to_completion(100, Duration::from_millis(1), None)
        .map_err(internal)?;
    let pages = backup.progress().pagecount;
    drop(backup);
    drop(target);
    file.sync_all().map_err(internal)?;
    Ok(object([
        ("format", IvyJson::Number(4.0)),
        ("pages", IvyJson::Number(f64::from(pages))),
    ]))
}
