//! Message attachments retain the original cross-platform filename policy.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, agent_schema::validate_agent};
use ivy_host_runtime::{filesystem, native_path};
use ivy_sdk::service::ToolHandler;
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex, Semaphore},
};

pub const MAXIMUM_BYTES: usize = 8 * 1024 * 1024;
pub const RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
pub const DECODE_CHARS: usize = 16 * 1024; // A multiple of four; only the final slice may contain padding.
pub const RETENTION_ENTRIES: usize = 32;
// One decoded/written upload per process, shared by all accounts. Do not queue large request bodies.
static UPLOAD: Semaphore = Semaphore::const_new(1);

#[derive(Default)]
struct Sweep {
    root: Option<PathBuf>,
    entries: Option<tokio::fs::ReadDir>,
    directory: Option<DirectorySweep>,
}
struct DirectorySweep {
    path: PathBuf,
    entries: tokio::fs::ReadDir,
    modified: SystemTime,
}
struct PartialFile {
    path: PathBuf,
    file: Option<tokio::fs::File>,
    published: bool,
}
impl Drop for PartialFile {
    fn drop(&mut self) {
        drop(self.file.take());
        if !self.published {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// The shared runtime supplies a Rust codec which accepts only canonical
/// standard base64. Invalid/noncanonical input returns None, including Unicode.
/// There is no production codec fallback in this domain owner.
pub type CanonicalBase64Decoder = dyn Fn(&JsString) -> Option<Vec<u8>> + Send + Sync;
pub type StagedClock = dyn Fn() -> SystemTime + Send + Sync;

pub struct StagedFiles {
    pub root: JsString,
    pub decode: Arc<CanonicalBase64Decoder>,
    pub now: Arc<StagedClock>,
    sweep: Mutex<Sweep>,
}
impl StagedFiles {
    pub fn new(root: JsString, decode: Arc<CanonicalBase64Decoder>) -> Self {
        Self {
            root,
            decode,
            now: Arc::new(SystemTime::now),
            sweep: Mutex::new(Sweep::default()),
        }
    }
    pub fn handler(self: &Arc<Self>) -> ToolHandler {
        let owner = self.clone();
        Arc::new(move |args, _| {
            let owner = owner.clone();
            Box::pin(async move {
                validate_agent("StageFileInput", &args)?;
                owner
                    .stage(
                        args.required("name")?.as_js_string().unwrap(),
                        args.required("dataBase64")?.as_js_string().unwrap(),
                    )
                    .await
            })
        })
    }
    pub async fn stage(&self, name: &JsString, encoded: &JsString) -> IvyResult<IvyJson> {
        let now = (self.now)();
        let invalid = || {
            IvyError::new(
                "invalid_arguments",
                "A staged file must be canonical base64 up to 8 MiB.",
            )
        };
        let length = encoded.0.len();
        let padding = encoded
            .0
            .iter()
            .rev()
            .take_while(|unit| **unit == 61)
            .count();
        if length > MAXIMUM_BYTES.div_ceil(3) * 4
            || length % 4 != 0
            || padding > 2
            || encoded.0[..length - padding].contains(&61)
            || length / 4 * 3 - padding > MAXIMUM_BYTES
        {
            return Err(invalid());
        }
        let _permit = UPLOAD.try_acquire().map_err(|_| {
            IvyError::new(
                "resource_exhausted",
                "Another file upload is being staged. Retry after it completes.",
            )
        })?;
        // Validate and hash bounded slices before touching storage. A second bounded pass writes
        // the same immutable input; neither the decoded file nor another full UTF-8 copy is retained.
        let mut digest = Sha256::new();
        let mut decoded_length = 0usize;
        for (index, chunk) in encoded.0.chunks(DECODE_CHARS).enumerate() {
            let bytes = (self.decode)(&JsString(chunk.to_vec())).ok_or_else(invalid)?;
            decoded_length = decoded_length
                .checked_add(bytes.len())
                .filter(|length| *length <= MAXIMUM_BYTES)
                .ok_or_else(invalid)?;
            digest.update(&bytes);
            // Keep control requests runnable while validating a maximum-size attachment.
            if index % 8 == 7 {
                tokio::task::yield_now().await;
            }
        }
        let mut hash = String::with_capacity(71);
        hash.push_str("sha256:");
        for byte in digest.finalize() {
            use std::fmt::Write;
            write!(hash, "{byte:02x}").unwrap();
        }
        let name = staged_name(name);
        let root = native_path::lexical(&native_path::to_native(&self.root));
        let directory = native_path::lexical(&root.join(&hash[7..]));
        let path = native_path::lexical(&directory.join(native_path::to_native(&name)));
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|_| IvyError::internal())?;
        let physical_root = tokio::fs::canonicalize(&root)
            .await
            .map_err(|_| IvyError::internal())?;
        match tokio::fs::create_dir(&directory).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(IvyError::internal()),
        }
        let metadata = tokio::fs::symlink_metadata(&directory)
            .await
            .map_err(|_| IvyError::internal())?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || tokio::fs::canonicalize(&directory)
                .await
                .map_err(|_| IvyError::internal())?
                .parent()
                != Some(physical_root.as_path())
        {
            return Err(IvyError::new(
                "forbidden",
                "A staged content directory must remain inside its owning root.",
            ));
        }
        let partial_path = directory.join(format!(".{}.part", uuid::Uuid::new_v4()));
        let output = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial_path)
            .await
            .map_err(|_| IvyError::internal())?;
        let mut partial = PartialFile {
            path: partial_path,
            file: Some(output),
            published: false,
        };
        for chunk in encoded.0.chunks(DECODE_CHARS) {
            let bytes = (self.decode)(&JsString(chunk.to_vec())).ok_or_else(invalid)?;
            partial
                .file
                .as_mut()
                .unwrap()
                .write_all(&bytes)
                .await
                .map_err(|_| IvyError::internal())?;
        }
        partial
            .file
            .as_mut()
            .unwrap()
            .flush()
            .await
            .map_err(|_| IvyError::internal())?;
        drop(partial.file.take());
        tokio::fs::rename(&partial.path, &path)
            .await
            .map_err(|_| IvyError::internal())?;
        partial.published = true;
        let time = chrono::DateTime::<chrono::Utc>::from(now).timestamp_millis();
        tokio::task::spawn_blocking(move || filesystem::set_times_ms(&directory, time, time))
            .await
            .map_err(|_| IvyError::internal())?
            .map_err(|_| IvyError::internal())?;
        self.retain(&physical_root, &physical_root.join(&hash[7..]), now)
            .await?;
        Ok(IvyJson::object([
            ("path", IvyJson::String(native_path::from_native(&path))),
            ("name", IvyJson::String(name)),
            ("byteLength", IvyJson::Number(decoded_length as f64)),
            ("contentHash", hash.into()),
        ]))
    }
    async fn retain(&self, root: &PathBuf, active: &PathBuf, now: SystemTime) -> IvyResult<()> {
        let mut sweep = self.sweep.lock().await;
        if sweep.root.as_ref() != Some(root) {
            sweep.root = Some(root.clone());
            sweep.entries = None;
            sweep.directory = None;
        }
        if sweep.entries.is_none() {
            sweep.entries = Some(
                tokio::fs::read_dir(root)
                    .await
                    .map_err(|_| IvyError::internal())?,
            );
        }
        // An upload may have renewed a directory since the preceding bounded slice.
        if let Some(directory) = &sweep.directory {
            let metadata = tokio::fs::symlink_metadata(&directory.path)
                .await
                .map_err(|_| IvyError::internal())?;
            if &directory.path == active
                || !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || metadata.modified().map_err(|_| IvyError::internal())? != directory.modified
            {
                sweep.directory = None;
            }
        }
        for _ in 0..RETENTION_ENTRIES {
            if let Some(directory) = sweep.directory.as_mut() {
                let entry = directory
                    .entries
                    .next_entry()
                    .await
                    .map_err(|_| IvyError::internal())?;
                if let Some(entry) = entry {
                    let metadata = tokio::fs::symlink_metadata(entry.path())
                        .await
                        .map_err(|_| IvyError::internal())?;
                    // Staging creates flat content directories. Never recurse into an unrelated tree.
                    if metadata.is_file() || metadata.file_type().is_symlink() {
                        filesystem::remove_force(entry.path()).await?;
                    }
                } else {
                    let path = sweep.directory.take().unwrap().path;
                    match tokio::fs::remove_dir(path).await {
                        Ok(()) => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::DirectoryNotEmpty
                                    | std::io::ErrorKind::NotFound
                            ) => {}
                        Err(_) => return Err(IvyError::internal()),
                    }
                }
                continue;
            }
            let entry = match sweep.entries.as_mut().unwrap().next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => {
                    sweep.entries = None;
                    break;
                }
                Err(_) => {
                    sweep.entries = None;
                    return Err(IvyError::internal());
                }
            };
            let metadata = tokio::fs::symlink_metadata(entry.path())
                .await
                .map_err(|_| IvyError::internal())?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() && expired(&metadata, now)? {
                sweep.directory = Some(DirectorySweep {
                    path: entry.path(),
                    modified: metadata.modified().map_err(|_| IvyError::internal())?,
                    entries: tokio::fs::read_dir(entry.path())
                        .await
                        .map_err(|_| IvyError::internal())?,
                });
            }
        }
        // Deleting children must not renew the retention age after a service restart.
        if let Some(directory) = sweep.directory.as_mut() {
            let path = directory.path.clone();
            let time = chrono::DateTime::<chrono::Utc>::from(directory.modified).timestamp_millis();
            tokio::task::spawn_blocking(move || filesystem::set_times_ms(&path, time, time))
                .await
                .map_err(|_| IvyError::internal())?
                .map_err(|_| IvyError::internal())?;
            directory.modified = tokio::fs::symlink_metadata(&directory.path)
                .await
                .map_err(|_| IvyError::internal())?
                .modified()
                .map_err(|_| IvyError::internal())?;
        }
        Ok(())
    }
}
fn expired(metadata: &std::fs::Metadata, now: SystemTime) -> IvyResult<bool> {
    Ok(now
        .duration_since(metadata.modified().map_err(|_| IvyError::internal())?)
        .is_ok_and(|age| age > RETENTION))
}
/// One path segment, keeping reserved names within the published UTF-16 cap.
pub fn staged_name(name: &JsString) -> JsString {
    let last = name
        .0
        .rsplit(|unit| matches!(*unit, 47 | 92))
        .next()
        .unwrap_or(&[]);
    let safe: Vec<_> = last
        .iter()
        .map(|unit| {
            if matches!(*unit, 0..=31 | 34 | 42 | 58 | 60 | 62 | 63 | 124) {
                95
            } else {
                *unit
            }
        })
        .collect();
    let edge = |unit: &u16| {
        *unit == 46 || char::from_u32(u32::from(*unit)).is_some_and(char::is_whitespace)
    };
    let start = safe
        .iter()
        .position(|unit| !edge(unit))
        .unwrap_or(safe.len());
    let end = safe
        .iter()
        .rposition(|unit| !edge(unit))
        .map_or(start, |index| index + 1);
    let mut units = safe[start..end]
        .iter()
        .take(128)
        .copied()
        .collect::<Vec<_>>();
    if units.is_empty() {
        return "file".into();
    }
    let first: Vec<_> = units
        .iter()
        .copied()
        .take_while(|unit| *unit != 46)
        .map(|unit| {
            if (65..=90).contains(&unit) {
                unit + 32
            } else {
                unit
            }
        })
        .collect();
    let reserved = ["con", "prn", "aux", "nul"]
        .iter()
        .any(|name| first.iter().copied().eq(name.encode_utf16()))
        || first.len() == 4
            && (first.starts_with(&[99, 111, 109]) || first.starts_with(&[108, 112, 116]))
            && (48..=57).contains(&first[3]);
    if reserved {
        units.insert(0, 95);
        units.truncate(128);
    }
    JsString(units)
}
