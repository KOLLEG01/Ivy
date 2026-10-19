//! Native filesystem/command boundary for Linux administration. Policy remains
//! in the resource/identity owners; alternate hosts are explicit test adapters.
use crate::{IvyError, IvyResult, exec_file, node_path, process};
use futures_util::future::BoxFuture;
use ivy_contracts::JsString;
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::PathBuf,
};

pub(crate) fn trimmed(value: &JsString) -> JsString {
    fn space(unit: u16) -> bool {
        matches!(unit,0x9..=0xd|0x20|0xa0|0x1680|0x2000..=0x200a|0x2028|0x2029|0x202f|0x205f|0x3000|0xfeff)
    }
    let start = value
        .0
        .iter()
        .position(|unit| !space(*unit))
        .unwrap_or(value.0.len());
    let end = value
        .0
        .iter()
        .rposition(|unit| !space(*unit))
        .map_or(start, |index| index + 1);
    JsString(value.0[start..end].to_vec())
}

#[derive(Clone, Debug)]
pub struct FileMetadata {
    pub is_file: bool,
    pub is_symlink: bool,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub mode: u32,
    pub size: u64,
}
pub trait LinuxHost: Send + Sync {
    fn platform(&self) -> &str;
    fn user_id(&self) -> Option<u32>;
    fn metadata(&self, path: JsString) -> BoxFuture<'static, io::Result<FileMetadata>>;
    /// Admission is eager: dropping the returned waiter does not cancel a read.
    fn read(&self, path: JsString) -> BoxFuture<'static, io::Result<Vec<u8>>>;
    /// Exclusive creation, file sync, then sync of the explicit parent.
    fn create(
        &self,
        path: JsString,
        bytes: Vec<u8>,
        mode: u32,
        parent: JsString,
    ) -> BoxFuture<'static, io::Result<()>>;
    fn command(
        &self,
        file: &'static str,
        args: Vec<JsString>,
        timeout_ms: u32,
        maximum_buffer_bytes: usize,
    ) -> BoxFuture<'static, IvyResult<JsString>>;
}
pub struct NativeLinuxHost {
    bootstrap_root: PathBuf,
}
impl NativeLinuxHost {
    pub fn new(bootstrap_root: PathBuf) -> Self {
        Self { bootstrap_root }
    }
}
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(io::Error::other)?
}
impl LinuxHost for NativeLinuxHost {
    fn platform(&self) -> &str {
        if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(windows) {
            "win32"
        } else {
            "other"
        }
    }
    fn user_id(&self) -> Option<u32> {
        #[cfg(unix)]
        {
            Some(unsafe { libc::getuid() })
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
    fn metadata(&self, path: JsString) -> BoxFuture<'static, io::Result<FileMetadata>> {
        Box::pin(blocking(move || {
            let value = fs::symlink_metadata(node_path::filesystem_path(&path))?;
            #[cfg(unix)]
            let (uid, gid, mode) = {
                use std::os::unix::fs::MetadataExt;
                (Some(value.uid()), Some(value.gid()), value.mode())
            };
            #[cfg(not(unix))]
            let (uid, gid, mode) = {
                let kind = if value.file_type().is_symlink() {
                    0o120000
                } else if value.is_dir() {
                    0o040000
                } else {
                    0o100000
                };
                let permission = if value.permissions().readonly() {
                    0o444
                } else {
                    0o666
                };
                (
                    Some(0),
                    Some(0),
                    kind | permission | if value.is_dir() { 0o111 } else { 0 },
                )
            };
            Ok(FileMetadata {
                is_file: value.is_file(),
                is_symlink: value.file_type().is_symlink(),
                uid,
                gid,
                mode,
                size: value.len(),
            })
        }))
    }
    fn read(&self, path: JsString) -> BoxFuture<'static, io::Result<Vec<u8>>> {
        let task = tokio::task::spawn_blocking(move || fs::read(node_path::filesystem_path(&path)));
        Box::pin(async move { task.await.map_err(io::Error::other)? })
    }
    fn create(
        &self,
        path: JsString,
        bytes: Vec<u8>,
        mode: u32,
        parent: JsString,
    ) -> BoxFuture<'static, io::Result<()>> {
        Box::pin(blocking(move || {
            let mut options = fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(mode);
            }
            #[cfg(not(unix))]
            let _ = mode;
            let mut file = options.open(node_path::filesystem_path(&path))?;
            let result = file.write_all(&bytes).and_then(|_| file.sync_all());
            drop(file);
            result?;
            fs::File::open(node_path::filesystem_path(&parent))?.sync_all()
        }))
    }
    fn command(
        &self,
        file: &'static str,
        args: Vec<JsString>,
        timeout_ms: u32,
        maximum_buffer_bytes: usize,
    ) -> BoxFuture<'static, IvyResult<JsString>> {
        let root = self.bootstrap_root.clone();
        Box::pin(async move {
            let result = exec_file::exec_file(
                exec_file::FileCommand {
                    executable: file.into(),
                    args: args
                        .into_iter()
                        .map(|arg| node_path::to_native(&arg).into_os_string())
                        .collect(),
                    cwd: None,
                    timeout_ms,
                    maximum_buffer_bytes,
                },
                root,
                exec_file::FileOptions {
                    environment: Some(process::runtime_environment(&BTreeMap::new())),
                    ..Default::default()
                },
            )
            .await?;
            if result.failure.is_some() {
                Err(IvyError::internal())
            } else {
                Ok(result.stdout)
            }
        })
    }
}
