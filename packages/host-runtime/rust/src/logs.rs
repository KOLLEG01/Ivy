//! Bounded process diagnostics. Work/data directories are never retention targets.
use crate::process::OutputStream;
use ivy_contracts::{IvyError, IvyResult};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub struct ServiceLog {
    current: PathBuf,
    previous: PathBuf,
}

fn error(_: std::io::Error) -> IvyError {
    IvyError::internal()
}
fn metadata(path: &Path) -> IvyResult<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(value) if value.is_file() && !value.file_type().is_symlink() => Ok(Some(value)),
        Ok(_) => Err(IvyError::new(
            "target_conflict",
            "Service log target is not a regular file.",
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(error(err)),
    }
}

impl ServiceLog {
    pub fn open(root: &Path) -> IvyResult<Self> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(root).map_err(error)?;
        let info = fs::symlink_metadata(root).map_err(error)?;
        if !info.is_dir() || info.file_type().is_symlink() {
            return Err(IvyError::new(
                "target_conflict",
                "Service log directory must not be a link.",
            ));
        }
        let value = Self {
            current: root.join("process.log"),
            previous: root.join("process.previous.log"),
        };
        metadata(&value.current)?;
        metadata(&value.previous)?;
        Ok(value)
    }

    pub fn append(&mut self, stream: OutputStream, text: &str, observed_at: &str) -> IvyResult<()> {
        let stream = match stream {
            OutputStream::Stdout => "stdout",
            OutputStream::Stderr => "stderr",
        };
        let record = format!("{observed_at} {stream} {text}").into_bytes();
        let bytes = &record[record.len().saturating_sub(65536)..];
        if metadata(&self.current)?.map_or(0, |value| value.len()) + bytes.len() as u64 > 1048576 {
            if metadata(&self.previous)?.is_some() {
                fs::remove_file(&self.previous).map_err(error)?;
            }
            if metadata(&self.current)?.is_some() {
                fs::rename(&self.current, &self.previous).map_err(error)?;
            }
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(&self.current)
            .map_err(error)?
            .write_all(bytes)
            .map_err(error)
    }
}
