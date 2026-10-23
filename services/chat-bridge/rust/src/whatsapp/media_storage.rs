//! Service-owned media files. Blocking filesystem work runs outside the reactor.
use ivy_contracts::{IvyError, IvyResult, JsString, hash::digest};
use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
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
}
impl MediaStorage {
    pub fn new(data_root: PathBuf) -> Self {
        Self {
            root: data_root.join("whatsapp-media"),
        }
    }
    pub fn directory(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }
    pub async fn create(&self, id: &str) -> IvyResult<()> {
        let directory = self.directory(id);
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
        let path = self.directory(id).join("original");
        blocking(move || match fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        })
        .await
    }
    pub async fn save_original(&self, id: &str, bytes: Vec<u8>) -> IvyResult<()> {
        let path = self.directory(id).join("original");
        blocking(move || write_new(&path, &bytes)).await
    }
    pub async fn document(
        &self,
        id: &str,
        name: &JsString,
        bytes: Vec<u8>,
    ) -> IvyResult<(ivy_contracts::IvyJson, bool)> {
        let directory = self.directory(id);
        let path = directory.join(format!("file-{}", name.to_utf8()?));
        let logical = path_json(&path)
            .into_js_string()
            .ok_or_else(IvyError::internal)?;
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
        let root = self.root.clone();
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
        let path = self.directory(id);
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
        let path = self.directory(id);
        blocking(move || match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        })
        .await
    }
}
