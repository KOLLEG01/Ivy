//! Host-only recovery of missing settings from a verified stopped-Hive snapshot.
use crate::storage_read::{STORAGE_FORMAT, snapshot_stopped_hive};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, ToSql, params, params_from_iter, types::Value,
};
use sha2::{Digest, Sha256};
use std::{ffi::OsString, fs::File, io::Read, path::Path, time::Duration};

pub const SETTINGS_CONTRACTS: [&str; 7] = [
    "ivy/host-configuration",
    "ivy/settings",
    "agent/instructions",
    "agent/mcp-configuration",
    "agent/skills",
    "secretary/configuration",
    "secretary/assignment",
];
const OBJECT_COLUMNS: &str = "id,parent_id,owner_object_id,name,path,position,icon,depth,contract_key,current_revision,archived_at,effective_archive,created_at,updated_at";
const REVISION_COLUMNS: &str = "object_id,revision,contract_key,contract_version,encoding,content,content_hash,byte_length,created_at,references_json";
fn internal<T>(_: T) -> IvyError {
    IvyError::internal()
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn rows(db: &Connection, sql: &str, values: &[&dyn ToSql]) -> IvyResult<Vec<Vec<Value>>> {
    let mut statement = db.prepare(sql).map_err(internal)?;
    let count = statement.column_count();
    statement
        .query_map(values, |row| (0..count).map(|i| row.get(i)).collect())
        .map_err(internal)?
        .collect::<Result<_, _>>()
        .map_err(internal)
}
fn row(db: &Connection, sql: &str, values: &[&dyn ToSql]) -> IvyResult<Option<Vec<Value>>> {
    Ok(rows(db, sql, values)?.into_iter().next())
}
fn text(value: &Value) -> IvyResult<&str> {
    if let Value::Text(value) = value {
        Ok(value)
    } else {
        Err(IvyError::internal())
    }
}
fn selected(db: &Connection) -> IvyResult<Vec<Vec<Value>>> {
    rows(
        db,
        &format!(
            "SELECT {OBJECT_COLUMNS} FROM objects WHERE contract_key IN (?,?,?,?,?,?,?) ORDER BY depth,id"
        ),
        &SETTINGS_CONTRACTS
            .iter()
            .map(|key| key as &dyn ToSql)
            .collect::<Vec<_>>(),
    )
}
fn revisions(db: &Connection, id: &str) -> IvyResult<Vec<Vec<Value>>> {
    rows(
        db,
        &format!("SELECT {REVISION_COLUMNS} FROM revisions WHERE object_id=? ORDER BY revision"),
        &[&id],
    )
}
fn open(path: &Path, writable: bool) -> IvyResult<Connection> {
    let db = Connection::open_with_flags(
        path,
        if writable {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        },
    )
    .map_err(internal)?;
    db.busy_timeout(Duration::from_secs(1)).map_err(internal)?;
    if !writable {
        db.execute_batch("PRAGMA query_only=ON; BEGIN")
            .map_err(internal)?;
    }
    Ok(db)
}
fn verified(db: &Connection) -> IvyResult<()> {
    let format: i64 = db
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(internal)?;
    require(
        format == STORAGE_FORMAT,
        "unsupported_storage_format",
        "Settings recovery requires the current Hive storage format.",
    )?;
    let checks = rows(db, "PRAGMA quick_check", &[])?;
    require(
        !checks.is_empty() && checks.iter().all(|row| row == &[Value::Text("ok".into())]),
        "storage_invalid",
        "Settings recovery refused an invalid Hive database.",
    )
}
fn hash(path: &Path) -> IvyResult<String> {
    let mut file = File::open(path).map_err(internal)?;
    let mut digest = Sha256::new();
    let mut bytes = [0u8; 65536];
    loop {
        let count = file.read(&mut bytes).map_err(internal)?;
        if count == 0 {
            break;
        }
        digest.update(&bytes[..count]);
    }
    Ok(format!(
        "sha256:{}",
        digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ))
}
fn source_hash(source: &Path, target: &Path, expected: &str) -> IvyResult<String> {
    require(
        expected.len() == 71
            && expected.starts_with("sha256:")
            && expected[7..]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid_arguments",
        "Select distinct databases and the exact backup hash.",
    )?;
    require(
        std::fs::canonicalize(source).map_err(internal)?
            != std::fs::canonicalize(target).map_err(internal)?,
        "invalid_arguments",
        "Select distinct databases and the exact backup hash.",
    )?;
    let actual = hash(source)?;
    require(
        actual == expected,
        "backup_hash_mismatch",
        "Settings backup hash does not match.",
    )?;
    Ok(actual)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Counts {
    restore: usize,
    preserve: usize,
    conflicts: usize,
}
impl Counts {
    fn json(&self) -> Json {
        Json::object([
            ("restore", (self.restore as f64).into()),
            ("preserve", (self.preserve as f64).into()),
            ("conflicts", (self.conflicts as f64).into()),
        ])
    }
}
#[derive(Debug, PartialEq, Eq)]
struct Plan {
    source_hash: String,
    total: usize,
    preserve_different: usize,
    counts: [Counts; 7],
}
impl Plan {
    fn new(source_hash: String) -> Self {
        Self {
            source_hash,
            total: 0,
            preserve_different: 0,
            counts: std::array::from_fn(|_| Counts::default()),
        }
    }
    fn conflicts(&self) -> usize {
        self.counts.iter().map(|c| c.conflicts).sum()
    }
    fn json(&self) -> Json {
        Json::object([
            ("format", (STORAGE_FORMAT as f64).into()),
            ("sourceHash", self.source_hash.clone().into()),
            ("total", (self.total as f64).into()),
            (
                "restore",
                (self.counts.iter().map(|c| c.restore).sum::<usize>() as f64).into(),
            ),
            (
                "preserve",
                (self.counts.iter().map(|c| c.preserve).sum::<usize>() as f64).into(),
            ),
            ("preserveDifferent", (self.preserve_different as f64).into()),
            ("conflicts", (self.conflicts() as f64).into()),
            (
                "byContract",
                Json::Object(
                    SETTINGS_CONTRACTS
                        .iter()
                        .zip(&self.counts)
                        .map(|(key, count)| ((*key).into(), count.json()))
                        .collect(),
                ),
            ),
        ])
    }
}
fn plan(source: &Connection, target: &Connection, source_hash: String) -> IvyResult<Plan> {
    verified(source)?;
    verified(target)?;
    let mut result = Plan::new(source_hash);
    for object in selected(source)? {
        let id = text(&object[0])?;
        let key = text(&object[8])?;
        let count = &mut result.counts[SETTINGS_CONTRACTS
            .iter()
            .position(|k| *k == key)
            .ok_or_else(IvyError::internal)?];
        result.total += 1;
        if let Some(existing) = row(
            target,
            "SELECT contract_key,parent_id,name,owner_object_id,path FROM objects WHERE id=?",
            &[&id],
        )? {
            if existing
                != [
                    object[8].clone(),
                    object[1].clone(),
                    object[3].clone(),
                    object[2].clone(),
                    object[4].clone(),
                ]
            {
                count.conflicts += 1;
            } else {
                count.preserve += 1;
                let source_current = row(
                    source,
                    "SELECT content_hash FROM revisions WHERE object_id=? AND revision=?",
                    &[&id, &object[9]],
                )?;
                let target_current = row(
                    target,
                    "SELECT content_hash FROM revisions WHERE object_id=? AND revision=(SELECT current_revision FROM objects WHERE id=?)",
                    &[&id, &id],
                )?;
                if source_current != target_current {
                    result.preserve_different += 1;
                }
            }
            continue;
        }
        let mut conflict =
            row(target, "SELECT id FROM objects WHERE path=?", &[&object[4]])?.is_some();
        for parent in [&object[1], &object[2]] {
            if parent != &Value::Null
                && row(target, "SELECT id FROM objects WHERE id=?", &[parent])?.is_none()
            {
                conflict = true;
            }
        }
        let family = row(
            source,
            "SELECT key,owner,media_type,object_mode FROM contract_families WHERE key=?",
            &[&key],
        )?;
        let current_family = row(
            target,
            "SELECT key,owner,media_type,object_mode FROM contract_families WHERE key=?",
            &[&key],
        )?;
        if family.is_none() || current_family.is_some() && current_family != family {
            conflict = true;
        }
        let history = revisions(source, id)?;
        if history.is_empty() || !history.iter().any(|r| r[1] == object[9]) {
            conflict = true;
        }
        for revision in history {
            if revision[9] != Value::Text("{}".into()) {
                conflict = true;
            }
            let definition = row(
                source,
                "SELECT definition FROM contracts WHERE key=? AND version=?",
                &[&key, &revision[3]],
            )?;
            let current = row(
                target,
                "SELECT definition FROM contracts WHERE key=? AND version=?",
                &[&key, &revision[3]],
            )?;
            if definition.is_none() || current.is_some() && current != definition {
                conflict = true;
            }
        }
        if conflict {
            count.conflicts += 1;
        } else {
            count.restore += 1;
        }
    }
    Ok(result)
}
fn insert_rows(target: &Connection, sql: &str, values: &[Vec<Value>]) -> IvyResult<()> {
    let mut statement = target.prepare(sql).map_err(internal)?;
    for row in values {
        statement.execute(params_from_iter(row)).map_err(internal)?;
    }
    Ok(())
}
fn restore(
    source_path: &Path,
    target_path: &Path,
    expected_hash: &str,
    safety_path: &Path,
) -> IvyResult<Json> {
    require(
        safety_path != source_path && safety_path != target_path,
        "invalid_arguments",
        "Safety backup path must be distinct.",
    )?;
    let source_hash = source_hash(source_path, target_path, expected_hash)?;
    let source = open(source_path, false)?;
    let preview = plan(&source, &open(target_path, false)?, source_hash.clone())?;
    require(
        preview.conflicts() == 0,
        "target_conflict",
        "Settings restore has unresolved contract, identity or reference conflicts.",
    )?;
    snapshot_stopped_hive(target_path, safety_path)?;
    let safety_hash = hash(safety_path)?;
    require(
        hash(source_path)? == source_hash,
        "backup_hash_mismatch",
        "Settings backup hash does not match.",
    )?;
    let target = open(target_path, true)?;
    target
        .execute_batch("PRAGMA foreign_keys=ON; PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE")
        .map_err(internal)?;
    let result = (|| {
        require(
            plan(&source, &target, source_hash)? == preview,
            "target_conflict",
            "Settings changed after the restore preview.",
        )?;
        for object in selected(&source)? {
            let id = text(&object[0])?;
            let key = text(&object[8])?;
            if row(&target, "SELECT id FROM objects WHERE id=?", &[&id])?.is_some() {
                continue;
            }
            if row(
                &target,
                "SELECT key FROM contract_families WHERE key=?",
                &[&key],
            )?
            .is_none()
            {
                insert_rows(
                    &target,
                    "INSERT INTO contract_families(key,owner,media_type,object_mode) VALUES (?,?,?,?)",
                    &rows(
                        &source,
                        "SELECT key,owner,media_type,object_mode FROM contract_families WHERE key=?",
                        &[&key],
                    )?,
                )?;
            }
            let history = revisions(&source, id)?;
            for revision in &history {
                if row(
                    &target,
                    "SELECT key FROM contracts WHERE key=? AND version=?",
                    &[&key, &revision[3]],
                )?
                .is_none()
                {
                    insert_rows(
                        &target,
                        "INSERT INTO contracts(key,version,definition) VALUES (?,?,?)",
                        &rows(
                            &source,
                            "SELECT key,version,definition FROM contracts WHERE key=? AND version=?",
                            &[&key, &revision[3]],
                        )?,
                    )?;
                }
            }
            target
                .execute(
                    &format!(
                        "INSERT INTO objects({OBJECT_COLUMNS}) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)"
                    ),
                    params_from_iter(&object),
                )
                .map_err(internal)?;
            insert_rows(
                &target,
                &format!("INSERT INTO revisions({REVISION_COLUMNS}) VALUES (?,?,?,?,?,?,?,?,?,?)"),
                &history,
            )?;
            let fts = row(
                &source,
                "SELECT name,text FROM object_fts WHERE object_id=?",
                &[&id],
            )?
            .unwrap_or_else(|| vec![object[3].clone(), Value::Text(String::new())]);
            target
                .execute(
                    "INSERT INTO object_fts(object_id,name,text) VALUES (?,?,?)",
                    params![id, fts[0], fts[1]],
                )
                .map_err(internal)?;
            let bytes: i64 = source
                .query_row(
                    "SELECT coalesce(sum(byte_length),0) FROM revisions WHERE object_id=?",
                    [id],
                    |row| row.get(0),
                )
                .map_err(internal)?;
            target.execute("UPDATE retention_families SET summary_json=json_set(summary_json,'$.objectCount',coalesce(json_extract(summary_json,'$.objectCount'),0)+1,'$.revisionCount',coalesce(json_extract(summary_json,'$.revisionCount'),0)+?,'$.byteLength',coalesce(json_extract(summary_json,'$.byteLength'),0)+?) WHERE contract_key=?",params![history.len() as i64,bytes,key]).map_err(internal)?;
        }
        verified(&target)?;
        let foreign_key_error: Option<String> = target
            .query_row("PRAGMA foreign_key_check", [], |row| row.get(0))
            .optional()
            .map_err(internal)?;
        require(
            foreign_key_error.is_none(),
            "storage_invalid",
            "Settings recovery refused an invalid Hive database.",
        )?;
        target.execute_batch("COMMIT").map_err(internal)?;
        let mut result = preview.json();
        result.insert("safetyHash", safety_hash.into());
        Ok(result)
    })();
    if !target.is_autocommit() {
        let _ = target.execute_batch("ROLLBACK");
    }
    result
}

pub fn command(args: &[OsString]) -> IvyResult<Json> {
    let usage = "Use settings-recovery backup SOURCE DEST, plan BACKUP STOPPED_TARGET HASH, or restore BACKUP STOPPED_TARGET HASH SAFETY_DEST.";
    require(args.len() >= 3, "invalid_arguments", usage)?;
    let source = Path::new(&args[1]);
    let target = Path::new(&args[2]);
    match args[0].to_str() {
        Some("backup") if args.len() == 3 => {
            let snapshot = snapshot_stopped_hive(source, target)?;
            let db = open(target, false)?;
            verified(&db)?;
            let mut result = Plan::new(hash(target)?);
            for object in selected(&db)? {
                result.total += 1;
                let key = text(&object[8])?;
                result.counts[SETTINGS_CONTRACTS
                    .iter()
                    .position(|item| *item == key)
                    .ok_or_else(IvyError::internal)?]
                .restore += 1;
            }
            let mut result = result.json();
            result.insert("pages", snapshot.required("pages")?.clone());
            Ok(result)
        }
        Some("plan") if args.len() == 4 => {
            let hash = source_hash(
                source,
                target,
                args[3].to_str().ok_or_else(IvyError::internal)?,
            )?;
            Ok(plan(&open(source, false)?, &open(target, false)?, hash)?.json())
        }
        Some("restore") if args.len() == 5 => restore(
            source,
            target,
            args[3].to_str().ok_or_else(IvyError::internal)?,
            Path::new(&args[4]),
        ),
        _ => Err(IvyError::new("invalid_arguments", usage)),
    }
}
