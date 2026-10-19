use super::{SystemdHost, UnitFile};
use crate::{
    IvyError, IvyJson, IvyResult, exec_file,
    linux_host::{FileMetadata, LinuxHost, NativeLinuxHost},
    node_path, process,
};
use futures_util::future::BoxFuture;
use ivy_contracts::JsString;
use std::{
    collections::BTreeMap,
    io,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::AsyncWriteExt;

pub struct NativeSystemdHost {
    root: PathBuf,
    files: NativeLinuxHost,
}
impl NativeSystemdHost {
    pub fn new(bootstrap_root: PathBuf) -> Self {
        Self {
            files: NativeLinuxHost::new(bootstrap_root.clone()),
            root: bootstrap_root,
        }
    }
}
struct NativeUnitFile(tokio::fs::File);
impl UnitFile for NativeUnitFile {
    fn write<'a>(&'a mut self, bytes: Vec<u8>) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move { self.0.write_all(&bytes).await })
    }
    fn sync(&mut self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move { self.0.sync_all().await })
    }
    fn close(self: Box<Self>) -> BoxFuture<'static, io::Result<()>> {
        Box::pin(async move {
            let file = self.0.into_std().await;
            tokio::task::spawn_blocking(move || {
                #[cfg(unix)]
                {
                    use std::os::fd::IntoRawFd;
                    if unsafe { libc::close(file.into_raw_fd()) } == 0 {
                        Ok(())
                    } else {
                        Err(io::Error::last_os_error())
                    }
                }
                #[cfg(windows)]
                {
                    use std::os::windows::io::IntoRawHandle;
                    if unsafe {
                        windows_sys::Win32::Foundation::CloseHandle(file.into_raw_handle())
                    } != 0
                    {
                        Ok(())
                    } else {
                        Err(io::Error::last_os_error())
                    }
                }
                #[cfg(not(any(unix, windows)))]
                {
                    drop(file);
                    Ok(())
                }
            })
            .await
            .map_err(io::Error::other)?
        })
    }
}
impl SystemdHost for NativeSystemdHost {
    fn platform(&self) -> &str {
        self.files.platform()
    }
    fn user_id(&self) -> Option<u32> {
        self.files.user_id()
    }
    fn metadata(&self, path: JsString) -> BoxFuture<'static, io::Result<FileMetadata>> {
        self.files.metadata(path)
    }
    fn read(&self, path: JsString) -> BoxFuture<'static, io::Result<Vec<u8>>> {
        self.files.read(path)
    }
    fn open(
        &self,
        path: JsString,
        exclusive_write: bool,
    ) -> BoxFuture<'static, io::Result<Box<dyn UnitFile>>> {
        Box::pin(async move {
            let mut options = tokio::fs::OpenOptions::new();
            if exclusive_write {
                options.create_new(true).write(true);
                #[cfg(unix)]
                options.mode(0o644);
            } else {
                options.read(true);
            }
            Ok(Box::new(NativeUnitFile(
                options.open(node_path::filesystem_path(&path)).await?,
            )) as Box<dyn UnitFile>)
        })
    }
    fn rename(&self, from: JsString, to: JsString) -> BoxFuture<'static, io::Result<()>> {
        Box::pin(async move {
            tokio::fs::rename(
                node_path::filesystem_path(&from),
                node_path::filesystem_path(&to),
            )
            .await
        })
    }
    fn unlink(&self, path: JsString) -> BoxFuture<'static, io::Result<()>> {
        Box::pin(async move { tokio::fs::remove_file(node_path::filesystem_path(&path)).await })
    }
    fn execute(
        &self,
        executable: JsString,
        args: Vec<JsString>,
    ) -> BoxFuture<'static, IvyResult<JsString>> {
        let root = self.root.clone();
        Box::pin(async move {
            let executed = exec_file::exec_file_detailed(
                exec_file::FileCommand {
                    executable: node_path::to_native(&executable).into_os_string(),
                    args: args
                        .iter()
                        .map(|arg| node_path::to_native(arg).into_os_string())
                        .collect(),
                    cwd: None,
                    timeout_ms: 30_000,
                    maximum_buffer_bytes: 65_536,
                },
                root,
                exec_file::FileOptions {
                    environment: Some(process::runtime_environment(&BTreeMap::new())),
                    ..Default::default()
                },
            )
            .await?;
            let Some(failure) = executed.result.failure else {
                return Ok(executed.result.stdout);
            };
            let code = match failure {
                exec_file::FileFailure::Spawn => spawn_code(
                    executed
                        .spawn_error
                        .as_ref()
                        .ok_or_else(IvyError::internal)?,
                )
                .into(),
                exec_file::FileFailure::MaxBuffer(_) => "ERR_CHILD_PROCESS_STDIO_MAXBUFFER".into(),
                exec_file::FileFailure::Exit => executed
                    .result
                    .exit_code
                    .map(|code| IvyJson::Number(code as f64))
                    .unwrap_or(IvyJson::Null),
                _ => IvyJson::Null,
            };
            let stderr = &executed.result.stderr.0;
            let detail = &stderr[stderr.len().saturating_sub(1024)..];
            let mut message = JsString::from("Systemd command failed.").0;
            if !detail.is_empty() {
                message.push(32);
                message.extend_from_slice(detail);
            }
            Err(IvyError::new_js("systemd_failed".into(), JsString(message))
                .with_outcome(ivy_contracts::Outcome::Unknown)
                .with_details(IvyJson::object([
                    (
                        "command",
                        IvyJson::String(
                            args.first().cloned().unwrap_or_else(|| "systemctl".into()),
                        ),
                    ),
                    ("exitCode", code),
                ])))
        })
    }
    fn uuid(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
    fn now_ms(&self) -> f64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(value) => value.as_millis() as f64,
            Err(error) => -(error.duration().as_millis() as f64),
        }
    }
    fn sleep(&self, milliseconds: u64) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(Duration::from_millis(milliseconds)))
    }
}

fn spawn_code(error: &io::Error) -> String {
    #[cfg(unix)]
    if let Some(code) = error.raw_os_error() {
        for (number, name) in [
            (libc::E2BIG, "E2BIG"),
            (libc::EACCES, "EACCES"),
            (libc::EAGAIN, "EAGAIN"),
            (libc::EBADF, "EBADF"),
            (libc::EBUSY, "EBUSY"),
            (libc::ECANCELED, "ECANCELED"),
            (libc::EFAULT, "EFAULT"),
            (libc::EINTR, "EINTR"),
            (libc::EINVAL, "EINVAL"),
            (libc::EIO, "EIO"),
            (libc::EISDIR, "EISDIR"),
            (libc::ELOOP, "ELOOP"),
            (libc::EMFILE, "EMFILE"),
            (libc::ENAMETOOLONG, "ENAMETOOLONG"),
            (libc::ENFILE, "ENFILE"),
            (libc::ENOBUFS, "ENOBUFS"),
            (libc::ENOENT, "ENOENT"),
            (libc::ENOMEM, "ENOMEM"),
            (libc::ENOSYS, "ENOSYS"),
            (libc::ENOTDIR, "ENOTDIR"),
            (libc::ENOTSUP, "ENOTSUP"),
            (libc::EPERM, "EPERM"),
            (libc::ETXTBSY, "ETXTBSY"),
        ] {
            if code == number {
                return name.into();
            }
        }
        return format!("Unknown system error {}", -code);
    }
    #[cfg(windows)]
    if let Some(code) = error.raw_os_error() {
        let name = match code {
            2 | 3 | 123 | 161 => Some("ENOENT"),
            5 | 740 | 1920 => Some("EACCES"),
            8 | 14 => Some("ENOMEM"),
            87 | 193 => Some("EINVAL"),
            206 => Some("ENAMETOOLONG"),
            267 => Some("ENOTDIR"),
            _ => None,
        };
        if let Some(name) = name {
            return name.into();
        }
    }
    match error.kind() {
        io::ErrorKind::NotFound => "ENOENT",
        io::ErrorKind::NotADirectory => "ENOTDIR",
        io::ErrorKind::PermissionDenied => "EACCES",
        io::ErrorKind::InvalidInput => "EINVAL",
        io::ErrorKind::ArgumentListTooLong => "E2BIG",
        io::ErrorKind::OutOfMemory => "ENOMEM",
        _ => "UNKNOWN",
    }
    .into()
}
