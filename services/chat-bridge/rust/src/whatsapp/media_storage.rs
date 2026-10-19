//! Service-owned media files. Blocking filesystem work runs outside the reactor.
use super::media::MediaDependencies;
use ivy_contracts::{IvyError, IvyResult, JsString, hash::digest};
use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    sync::Arc,
    time::UNIX_EPOCH,
};

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> IvyResult<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| IvyError::internal())?
        .map_err(|_| IvyError::internal())
}
pub(super) fn path_json(path: &std::path::Path) -> ivy_contracts::IvyJson {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        ivy_contracts::IvyJson::String(JsString(path.as_os_str().encode_wide().collect()))
    }
    #[cfg(not(windows))]
    {
        path.to_string_lossy().as_ref().into()
    }
}
fn write_new(path: &std::path::Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}

#[derive(Clone)]
pub(super) struct MediaStorage {
    root: PathBuf,
    dependencies: Arc<dyn MediaDependencies>,
}
impl MediaStorage {
    pub fn new(data_root: PathBuf, dependencies: Arc<dyn MediaDependencies>) -> Self {
        Self {
            root: data_root.join("whatsapp-media"),
            dependencies,
        }
    }
    pub fn directory(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }
    fn path(&self, path: &std::path::Path) -> IvyResult<PathBuf> {
        self.dependencies.filesystem_path(
            &path_json(path)
                .into_js_string()
                .ok_or_else(IvyError::internal)?,
        )
    }
    pub async fn create(&self, id: &str) -> IvyResult<()> {
        let directory = self.path(&self.directory(id))?;
        blocking(move || {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(directory)
        })
        .await
    }
    pub async fn original(&self, id: &str) -> IvyResult<Option<Vec<u8>>> {
        let path = self.path(&self.directory(id).join("original"))?;
        blocking(move || match fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        })
        .await
    }
    pub async fn save_original(&self, id: &str, bytes: Vec<u8>) -> IvyResult<()> {
        let path = self.path(&self.directory(id).join("original"))?;
        blocking(move || write_new(&path, &bytes)).await
    }
    pub async fn document(
        &self,
        id: &str,
        name: &JsString,
        bytes: Vec<u8>,
    ) -> IvyResult<(ivy_contracts::IvyJson, bool)> {
        let mut leaf = JsString::from("file-");
        leaf.0.extend_from_slice(&name.0);
        let directory = self.directory(id);
        let mut logical = path_json(&directory)
            .into_js_string()
            .ok_or_else(IvyError::internal)?;
        logical.0.push(std::path::MAIN_SEPARATOR as u16);
        logical.0.extend_from_slice(&leaf.0);
        let path = self.dependencies.filesystem_path(&logical)?;
        blocking(move || {
            let same = match write_new(&path, &bytes) {
                Ok(()) => true,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    digest(fs::read(&path)?) == digest(&bytes)
                }
                Err(e) => return Err(e),
            };
            Ok((ivy_contracts::IvyJson::String(logical), same))
        })
        .await
    }
    pub async fn entries(&self) -> IvyResult<Vec<String>> {
        let root = self.path(&self.root)?;
        blocking(move || {
            let entries = match fs::read_dir(root) {
                Ok(entries) => entries,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(e) => return Err(e),
            };
            let mut names = Vec::new();
            for entry in entries {
                let entry = entry?;
                let kind = entry.file_type()?;
                if !kind.is_dir() || kind.is_symlink() {
                    continue;
                }
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.len() == 64
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    names.push(name.into_owned());
                }
            }
            names.sort();
            Ok(names)
        })
        .await
    }
    pub async fn modified_ms(&self, id: &str) -> IvyResult<f64> {
        let path = self.path(&self.directory(id))?;
        blocking(move || {
            let modified = fs::metadata(path)?.modified()?;
            Ok(match modified.duration_since(UNIX_EPOCH) {
                Ok(d) => d.as_secs_f64() * 1000.0,
                Err(e) => -e.duration().as_secs_f64() * 1000.0,
            })
        })
        .await
    }
    pub async fn remove(&self, id: &str) -> IvyResult<()> {
        let path = self.path(&self.directory(id))?;
        blocking(move || match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        })
        .await
    }
}
