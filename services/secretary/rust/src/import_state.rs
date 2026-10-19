//! One-time core backup, scope-bound admission and bounded import receipts.
//! None of these conversions run from the normal journal constructor.
use crate::journal::{Journal, Limits};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::{digest, hash_json},
    json::{canonical, parse},
    operation::parse_operation_id_js,
};
use ivy_host_runtime::filesystem::{stat_identity, stat_link_count};
use ivy_store::{
    rusqlite::{Connection, OpenFlags, OptionalExtension, params},
    sqlite,
};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

pub(crate) const FORMAT: &str = "secretary/current-format";
pub(crate) const RUN: &str = "secretary/import/run";
const FRAME: usize = 16 * 1024 * 1024;
const CHUNK: usize = 1024 * 1024;
pub(crate) fn error(code: &str, message: &str) -> IvyError {
    IvyError::new(code, message)
}
pub fn import_identity(settings: &IvyJson, service_node_id: &JsString) -> IvyResult<IvyJson> {
    let identity = settings.required("identity")?;
    Ok(IvyJson::object([
        ("scope", identity.required("scope")?.clone()),
        ("principalId", identity.required("principalId")?.clone()),
        ("serviceNodeId", IvyJson::String(service_node_id.clone())),
    ]))
}
fn state_table(journal: &Journal) -> IvyResult<bool> {
    journal.db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='journal_state')",[],|r|r.get(0)).map_err(sqlite::error)
}
pub(crate) fn read(journal: &Journal, key: &str) -> IvyResult<Option<IvyJson>> {
    if !state_table(journal)? {
        return Ok(None);
    }
    let text: Option<String> = journal
        .db
        .query_row("SELECT value FROM journal_state WHERE key=?", [key], |r| {
            r.get(0)
        })
        .optional()
        .map_err(sqlite::error)?;
    text.map(|text| parse(&text, FRAME)).transpose()
}
/// Each imported record/receipt is bounded by the existing journal limits. No
/// quota cleanup, expiry fence or original revision is modified to make room.
pub(crate) fn save(journal: &Journal, key: &str, value: &IvyJson) -> IvyResult<()> {
    let encoded = canonical(value, FRAME)?;
    let (count,bytes):(i64,i64)=journal.db.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(value AS BLOB))),0) FROM journal_state WHERE key LIKE 'secretary/import/%' AND key<>?",[key],|r|Ok((r.get(0)?,r.get(1)?))).map_err(sqlite::error)?;
    if count >= journal.limits.count || bytes + encoded.len() as i64 > journal.limits.bytes {
        return Err(error(
            "capacity_exceeded",
            "The explicit import receipt budget is full; original data remains intact.",
        ));
    }
    journal.db.execute("INSERT INTO journal_state(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,encoded]).map_err(sqlite::error)?;
    Ok(())
}
fn same(a: &IvyJson, b: &IvyJson) -> IvyResult<bool> {
    Ok(canonical(a, FRAME)? == canonical(b, FRAME)?)
}
fn check_identity(value: &IvyJson, identity: &IvyJson) -> IvyResult<()> {
    if value.get("schemaVersion") == Some(&IvyJson::Number(1.0))
        && same(value.required("identity")?, identity)?
    {
        Ok(())
    } else {
        Err(error(
            "secretary_import_identity_conflict",
            "The journal import belongs to another Secretary scope or owner.",
        ))
    }
}
pub fn current(journal: &Journal, identity: &IvyJson) -> IvyResult<bool> {
    let Some(marker) = read(journal, FORMAT)? else {
        return Ok(false);
    };
    check_identity(&marker, identity)?;
    if marker.get("complete") != Some(&true.into()) {
        return Err(error(
            "secretary_import_required",
            "Retained Secretary data needs its explicit completed import.",
        ));
    }
    Ok(true)
}
/// Admission precedes engine construction, initialization and worker effects.
/// An existing unmarked file is never silently treated as an empty installation.
pub fn runtime_journal(path: &Path, limits: Limits, identity: &IvyJson) -> IvyResult<Journal> {
    let fresh = match fs::metadata(path) {
        Ok(_) => false,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => {
            return Err(error(
                "storage_unavailable",
                "The Secretary journal could not be inspected.",
            ));
        }
    };
    let journal = Journal::claim_exclusive(path, limits)?;
    if current(&journal, identity)? {
        journal.initialize_schema()?;
        return Ok(journal);
    }
    if !fresh {
        return Err(error(
            "secretary_import_required",
            "Retained Secretary data needs the explicit import command before startup.",
        ));
    }
    journal.initialize_schema()?;
    save(
        &journal,
        FORMAT,
        &IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("identity", identity.clone()),
            ("complete", true.into()),
            ("source", IvyJson::Null),
        ]),
    )?;
    Ok(journal)
}
fn regular(path: &Path) -> IvyResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        error(
            "storage_unavailable",
            "The import file could not be inspected.",
        )
    })?;
    #[cfg(windows)]
    let reparse = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let reparse = false;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || reparse
        || stat_link_count(path).map_err(|_| IvyError::internal())? != 1
    {
        return Err(error(
            "secretary_import_backup_invalid",
            "Use distinct regular core and backup files.",
        ));
    }
    Ok(())
}
fn manifest(path: &Path, name: &str) -> IvyResult<IvyJson> {
    regular(path)?;
    let mut file = File::open(path).map_err(|_| {
        error(
            "storage_unavailable",
            "The import backup could not be read.",
        )
    })?;
    let mut buffer = vec![0; CHUNK];
    let mut chunks = vec![];
    let mut length = 0u64;
    loop {
        let mut filled = 0;
        while filled < buffer.len() {
            let n = file
                .read(&mut buffer[filled..])
                .map_err(|_| error("storage_unavailable", "The import backup read failed."))?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            break;
        }
        length += filled as u64;
        chunks.push(digest(&buffer[..filled]).into());
        // Bounded manifest, without allocating the journal file in memory.
        if chunks.len() > 100_000 {
            return Err(error(
                "limit_exceeded",
                "The import backup manifest is too large.",
            ));
        }
    }
    Ok(IvyJson::object([
        ("filename", name.into()),
        ("byteLength", IvyJson::Number(length as f64)),
        ("chunkBytes", IvyJson::Number(CHUNK as f64)),
        ("chunks", IvyJson::Array(chunks)),
    ]))
}
fn backup(journal: &Journal, path: &Path, name: &str) -> IvyResult<IvyJson> {
    regular(path)?;
    let (busy, _, _): (i64, i64, i64) = journal
        .db
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(sqlite::error)?;
    if busy != 0 {
        return Err(error(
            "secretary_import_owner_busy",
            "Stop the original journal owner before importing.",
        ));
    }
    let destination = path.parent().ok_or_else(IvyError::internal)?.join(name);
    let expected = manifest(path, name)?;
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
    {
        Ok(mut target) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                target
                    .set_permissions(fs::Permissions::from_mode(0o600))
                    .map_err(|_| IvyError::internal())?;
            }
            let mut source = File::open(path).map_err(|_| IvyError::internal())?;
            std::io::copy(&mut source, &mut target).map_err(|_| {
                error(
                    "storage_unavailable",
                    "The import backup copy did not finish; its file is retained.",
                )
            })?;
            target.flush().map_err(|_| IvyError::internal())?;
            target.sync_all().map_err(|_| IvyError::internal())?;
            #[cfg(unix)]
            File::open(destination.parent().unwrap())
                .and_then(|d| d.sync_all())
                .map_err(|_| IvyError::internal())?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => {
            return Err(error(
                "storage_unavailable",
                "The import backup could not be created.",
            ));
        }
    }
    if stat_identity(path).map_err(|_| IvyError::internal())?
        == stat_identity(&destination).map_err(|_| IvyError::internal())?
        || !same(&manifest(&destination, name)?, &expected)?
    {
        return Err(error(
            "secretary_import_backup_changed",
            "The original backup file is incomplete or changed; it was not overwritten.",
        ));
    }
    Ok(IvyJson::object([
        ("manifestHash", hash_json(&expected)?.into()),
        ("manifest", expected),
    ]))
}
fn verify_backup(path: &Path, source: &IvyJson) -> IvyResult<()> {
    let expected = source.required("manifest")?;
    let name = expected.required("filename")?.as_str()?;
    if name.contains('/')
        || name.contains('\\')
        || !name.starts_with("secretary-import-")
        || !name.ends_with(".sqlite")
    {
        return Err(error(
            "secretary_import_backup_invalid",
            "The retained backup filename is invalid.",
        ));
    }
    let actual = manifest(
        &path.parent().ok_or_else(IvyError::internal)?.join(&name),
        &name,
    )?;
    if !same(expected, &actual)? || source.get("manifestHash") != Some(&hash_json(&actual)?.into())
    {
        return Err(error(
            "secretary_import_backup_changed",
            "The sealed import backup changed.",
        ));
    }
    Ok(())
}
pub struct PreparedImport {
    pub journal: Journal,
    pub state: Option<IvyJson>,
}
/// The sealed baseline is a standalone checkpointed file. Immutable URI reads
/// neither create sidecars nor accept later WAL writes as part of that baseline.
pub fn open_source(path: &Path) -> IvyResult<Connection> {
    let mut uri = url::Url::from_file_path(path).map_err(|_| {
        error(
            "secretary_import_backup_invalid",
            "The backup needs an absolute native path.",
        )
    })?;
    uri.set_query(Some("immutable=1"));
    Connection::open_with_flags(
        uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sqlite::error)
}
/// Called before creating the import's Hive connection. No workers start here.
pub fn prepare(
    path: &Path,
    limits: Limits,
    identity: &IvyJson,
    operation: &JsString,
    now: i64,
) -> IvyResult<PreparedImport> {
    parse_operation_id_js(operation)?;
    let journal = Journal::claim_exclusive(path, limits)?;
    if current(&journal, identity)? {
        return Ok(PreparedImport {
            journal,
            state: None,
        });
    }
    if let Some(state) = read(&journal, RUN)? {
        check_identity(&state, identity)?;
        if state.get("operationId") != Some(&IvyJson::String(operation.clone())) {
            return Err(error(
                "secretary_import_identity_conflict",
                "Resume this import using its original operation ID.",
            ));
        }
        verify_backup(path, state.required("source")?)?;
        return Ok(PreparedImport {
            journal,
            state: Some(state),
        });
    }
    let name = format!(
        "secretary-import-{}.sqlite",
        &hash_json(&IvyJson::String(operation.clone()))?[7..]
    );
    let source = backup(&journal, path, &name)?;
    journal.initialize_schema()?;
    let state = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("identity", identity.clone()),
        ("operationId", IvyJson::String(operation.clone())),
        ("source", source),
        ("at", IvyJson::Number(now as f64)),
        ("stage", "follow-up".into()),
        ("cursor", IvyJson::Null),
    ]);
    save(&journal, RUN, &state)?;
    Ok(PreparedImport {
        journal,
        state: Some(state),
    })
}
pub(crate) fn publish(journal: &Journal, state: &IvyJson) -> IvyResult<()> {
    save(
        journal,
        FORMAT,
        &IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("identity", state.required("identity")?.clone()),
            ("complete", true.into()),
            ("source", state.required("source")?.clone()),
            ("operationId", state.required("operationId")?.clone()),
        ]),
    )
}
