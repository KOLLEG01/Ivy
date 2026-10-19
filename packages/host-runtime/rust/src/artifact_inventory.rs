//! Contained-file inventory for package comparison and immutable source identity.
use crate::{
    IvyError, IvyJson, IvyResult, concurrency::map_concurrent, node_path, paths, streaming_hash,
};
use ivy_contracts::{JsString, hash::hash_json};
use std::{
    cmp::Ordering,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// The executing Node default locale is supplied by the shared runtime binding.
/// This owner does not substitute ASCII or Rust string ordering for localeCompare.
pub type FileCompare = Arc<dyn Fn(&JsString, &JsString) -> Ordering + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactFile {
    pub path: JsString,
    pub hash: String,
    pub bytes: u64,
    pub mode: u32,
}
impl ArtifactFile {
    pub fn json(&self) -> IvyJson {
        let mut value = self.identity();
        value.insert("mode", IvyJson::Number(self.mode as f64));
        value
    }
    fn identity(&self) -> IvyJson {
        IvyJson::object([
            ("path", IvyJson::String(self.path.clone())),
            ("hash", self.hash.clone().into()),
            ("bytes", IvyJson::Number(self.bytes as f64)),
        ])
    }
}

/// Candidate source identity deliberately excludes checkout permission bits.
pub fn artifact_source_identity(files: &[ArtifactFile]) -> IvyResult<String> {
    hash_json(&IvyJson::Array(
        files.iter().map(ArtifactFile::identity).collect(),
    ))
}
fn internal<T, E>(result: Result<T, E>) -> IvyResult<T> {
    result.map_err(|_| IvyError::internal())
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn fs_path(path: &Path) -> PathBuf {
    node_path::filesystem_path(&node_path::from_native(path))
}
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> IvyResult<T> + Send + 'static,
) -> IvyResult<T> {
    internal(tokio::task::spawn_blocking(work).await)?
}
fn mode(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.mode() & 0o777
    }
    #[cfg(windows)]
    {
        let access = if metadata.permissions().readonly() {
            0o444
        } else {
            0o666
        };
        access | if metadata.is_dir() { 0o111 } else { 0 }
    }
}

/// Observe links/special entries before hashing, then drain each four-worker batch.
/// Admitted inventory work remains owned if the caller stops waiting.
pub async fn artifact_files(root: PathBuf, compare: FileCompare) -> IvyResult<Vec<ArtifactFile>> {
    internal(tokio::spawn(inventory(root, compare)).await)?
}
async fn inventory(root: PathBuf, compare: FileCompare) -> IvyResult<Vec<ArtifactFile>> {
    let mut directories = vec![root.clone()];
    let files = Arc::new(Mutex::new(Vec::new()));
    let mut index = 0;
    while index < directories.len() {
        let end = (index + 4).min(directories.len());
        let batch = directories[index..end].to_vec();
        index = end;
        let nested = map_concurrent(batch, 4, |directory, _| {
            let root = root.clone();
            let files = files.clone();
            blocking(move || {
                let mut entries = internal(fs::read_dir(fs_path(&directory)))?
                    .map(|entry| internal(entry))
                    .collect::<IvyResult<Vec<_>>>()?;
                // libuv scandir sorts native Unix filename bytes before decoding.
                #[cfg(unix)]
                entries.sort_by(|left, right| {
                    use std::os::unix::ffi::OsStrExt;
                    left.file_name()
                        .as_bytes()
                        .cmp(right.file_name().as_bytes())
                });
                let mut nested = Vec::new();
                let mut found = Vec::new();
                for entry in entries {
                    let name = node_path::to_native(&node_path::node_utf8(&entry.file_name()));
                    let path = directory.join(name);
                    let local = internal(node_path::Context::current())?.relative(
                        &node_path::from_native(&root),
                        &node_path::from_native(&path),
                    );
                    let local = JsString(
                        local
                            .0
                            .into_iter()
                            .map(|unit| if unit == 92 { 47 } else { unit })
                            .collect(),
                    );
                    let kind = internal(entry.file_type())?;
                    require(
                        internal(paths::inside(&root, &path))? && !kind.is_symlink(),
                        "artifact_changed",
                        "Package comparison accepts contained files and directories only.",
                    )?;
                    if kind.is_dir() {
                        nested.push(path);
                    } else {
                        require(
                            kind.is_file(),
                            "artifact_changed",
                            "Package comparison found an unsupported entry.",
                        )?;
                        found.push((path, local));
                    }
                }
                // Preserve the completion order of directory callbacks before the
                // final stable locale sort, including equal comparison keys.
                internal(files.lock())?.extend(found);
                Ok(nested)
            })
        })
        .await?;
        for children in nested {
            directories.extend(children);
        }
    }
    let files = std::mem::take(&mut *internal(files.lock())?);
    require(
        files.len() <= 50_000,
        "limit_exceeded",
        "Package comparison exceeds its file limit.",
    )?;
    let mut result = map_concurrent(files, 4, |(path, local), _| {
        blocking(move || {
            let metadata = internal(fs::symlink_metadata(fs_path(&path)))?;
            Ok(ArtifactFile {
                path: local,
                hash: streaming_hash::hash_file(&fs_path(&path))?,
                bytes: metadata.len(),
                mode: mode(&metadata),
            })
        })
    })
    .await?;
    result.sort_by(|left, right| compare(&left.path, &right.path));
    Ok(result)
}
