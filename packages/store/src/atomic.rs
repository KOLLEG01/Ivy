use ivy_contracts::{
    IvyError, IvyJson, IvyResult, Outcome, json::canonical, limits::MANAGEMENT_FRAME_BYTES,
};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

fn io_error(error: std::io::Error) -> IvyError {
    IvyError::new("storage_unavailable", error.to_string()).with_outcome(Outcome::Unknown)
}

/// Synchronous durable replacement. Call on a blocking worker, never the async reactor.
/// Readers denying Windows delete sharing keep the old destination intact throughout retries.
pub fn replace_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut attempt = 0;
    loop {
        match fs::rename(temporary, destination) {
            Ok(()) => return Ok(()),
            Err(error) => {
                if !cfg!(windows)
                    || !matches!(error.raw_os_error(), Some(5 | 32 | 33))
                    || Instant::now() >= deadline
                {
                    return Err(error);
                }
                let pause = Duration::from_millis((10u64 << attempt.min(4)).min(100))
                    .min(deadline.saturating_duration_since(Instant::now()));
                std::thread::sleep(pause);
                attempt += 1;
            }
        }
    }
}
pub fn atomic_json(path: &Path, value: &IvyJson) -> IvyResult<()> {
    let mut bytes = canonical(value, MANAGEMENT_FRAME_BYTES)?.into_bytes();
    bytes.push(b'\n');
    atomic_write(path, &bytes).map_err(io_error)
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let directory = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(directory)?;
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let temporary = std::path::PathBuf::from(temporary);
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        {
            let mut file = options.open(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        replace_file(&temporary, path)?;
        #[cfg(unix)]
        {
            fs::File::open(directory)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
