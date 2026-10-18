//! Durable stop uncertainty. Only external ownership reconciliation clears a fence.
use crate::{config::read_json, IvyError, IvyJson, IvyResult};
use ivy_contracts::{core_schema::validate_host, json, Outcome};
use std::{fs, io::Write, path::{Path, PathBuf}};

fn path(root: &Path) -> PathBuf { root.join("process-stop-fence.json") }
fn unknown(message: &str) -> IvyError {
    IvyError::new("outcome_unknown", message).with_outcome(Outcome::Unknown)
}
fn observe(root: &Path) -> IvyResult<Option<IvyJson>> {
    let path = path(root);
    match fs::symlink_metadata(&path) {
        Ok(_) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(unknown("The process stop fence cannot be observed safely.")),
    }
    let invalid = || unknown("The process stop fence cannot be verified; reconcile its owned runtime data before another launch.");
    let fence = read_json(&path, 4096).map_err(|_| invalid())?;
    validate_host("ProcessStopFence", &fence).map_err(|_| invalid())?;
    Ok(Some(fence))
}

pub async fn process_stop_fence(data_root: impl Into<PathBuf>) -> IvyResult<Option<IvyJson>> {
    let root = data_root.into();
    tokio::task::spawn_blocking(move || observe(&root)).await.map_err(|_| IvyError::internal())?
}

pub async fn require_clear_process_stop_fence(data_root: impl Into<PathBuf>) -> IvyResult<()> {
    if let Some(fence) = process_stop_fence(data_root).await? {
        return Err(unknown("A prior process tree did not establish its stopped outcome; reconcile its owned runtime before another launch.")
            .with_details(IvyJson::object(["owner", "ownerId", "processId", "observedAt"].map(|key| (key, fence.get(key).unwrap().clone())))));
    }
    Ok(())
}

/// Exclusive creation preserves a prior marker even if its contents are invalid.
pub async fn record_process_stop_fence(data_root: impl Into<PathBuf>, fence: IvyJson) -> IvyResult<bool> {
    validate_host("ProcessStopFence", &fence)?;
    let root = data_root.into();
    tokio::task::spawn_blocking(move || {
        let mut directory = fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)] {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        directory.create(&root).map_err(|_| IvyError::internal())?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(path(&root)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(_) => return Err(IvyError::internal()),
        };
        let mut bytes = json::canonical(&fence, usize::MAX)?.into_bytes();
        bytes.push(b'\n');
        file.write_all(&bytes).and_then(|_| file.sync_all()).map_err(|_| IvyError::internal())?;
        drop(file);
        #[cfg(unix)]
        fs::File::open(&root).and_then(|file| file.sync_all()).map_err(|_| IvyError::internal())?;
        Ok(true)
    }).await.map_err(|_| IvyError::internal())?
}

// ECMAScript trim includes BOM and excludes NEL, unlike Rust's Unicode trim.
fn js_space(ch: char) -> bool {
    matches!(ch, '\u{9}'..='\u{d}' | '\u{20}' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

/// Call only after external owner-release proof for this exact fence has passed.
pub async fn clear_process_stop_fence(data_root: impl Into<PathBuf>, expected: IvyJson) -> IvyResult<bool> {
    validate_host("ProcessStopFence", &expected)?;
    let root = data_root.into();
    tokio::task::spawn_blocking(move || {
        let path = path(&root);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(IvyError::internal()),
        };
        if String::from_utf8_lossy(&bytes).trim_matches(js_space) != json::canonical(&expected, usize::MAX)? { return Ok(false); }
        fs::remove_file(path).map_err(|_| IvyError::internal())?;
        Ok(true)
    }).await.map_err(|_| IvyError::internal())?
}
