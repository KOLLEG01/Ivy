//! Private durable-maintenance directories, shared by bootstrap and backups.
use crate::{IvyError, IvyResult, paths};
use std::path::{Path, PathBuf};

pub async fn exists(path: &Path) -> IvyResult<bool> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(IvyError::internal()),
    }
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
pub async fn private_directory(path: PathBuf, job_launcher: PathBuf) -> IvyResult<()> {
    require(
        path.is_absolute(),
        "invalid_arguments",
        "Private backup/restore directories require absolute paths.",
    )?;
    let parent = path.parent().ok_or_else(IvyError::internal)?;
    let parent = paths::realpath(parent).map_err(|_| IvyError::internal())?;
    require(
        tokio::fs::symlink_metadata(&parent)
            .await
            .map_err(|_| IvyError::internal())?
            .is_dir()
            && !exists(&path).await?,
        "target_conflict",
        "Backup/restore needs a fresh directory.",
    )?;
    let builder = tokio::fs::DirBuilder::new();
    #[cfg(unix)]
    let mut builder = builder;
    #[cfg(unix)]
    builder.mode(0o700);
    builder
        .create(&path)
        .await
        .map_err(|_| IvyError::internal())?;
    let actual = paths::realpath(&path).map_err(|_| IvyError::internal())?;
    require(
        actual.parent() == Some(parent.as_path())
            && !tokio::fs::symlink_metadata(&path)
                .await
                .map_err(|_| IvyError::internal())?
                .file_type()
                .is_symlink(),
        "target_conflict",
        "Private directory ownership changed.",
    )?;
    #[cfg(windows)]
    {
        use crate::{
            exec_file::{self, FileCommand, FileOptions},
            process,
        };
        let system =
            PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
                .join("System32");
        let command = |name: &str, args, maximum_buffer_bytes| FileCommand {
            executable: system.join(name).into_os_string(),
            args,
            cwd: None,
            timeout_ms: 10_000,
            maximum_buffer_bytes,
        };
        let options = || FileOptions {
            environment: Some(process::runtime_environment(&Default::default())),
            job_launcher: Some(job_launcher.clone()),
            ..Default::default()
        };
        let result = exec_file::exec_file(
            command(
                "whoami.exe",
                vec!["/user".into(), "/fo".into(), "csv".into(), "/nh".into()],
                1024 * 1024,
            ),
            system.clone(),
            options(),
        )
        .await?;
        if result.failure.is_some() {
            return Err(IvyError::internal());
        }
        let output = result.stdout.to_utf8()?;
        let sid = output.match_indices("S-1-").find_map(|(start, _)| {
            let rest = &output[start + 4..];
            let length = rest
                .bytes()
                .take_while(|c| c.is_ascii_digit() || *c == b'-')
                .count();
            let sid = &output[start..start + 4 + length];
            let groups = sid[4..].split('-').collect::<Vec<_>>();
            (groups.len() >= 2
                && groups
                    .iter()
                    .all(|v| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit())))
            .then_some(sid)
        });
        require(
            sid.is_some(),
            "private_storage_unavailable",
            "Current private storage principal could not be established.",
        )?;
        let args = vec![
            path.into_os_string(),
            "/inheritance:r".into(),
            "/grant:r".into(),
            format!("*{}:(OI)(CI)F", sid.unwrap()).into(),
            "*S-1-5-18:(OI)(CI)F".into(),
            "*S-1-5-32-544:(OI)(CI)F".into(),
        ];
        let result = exec_file::exec_file(
            command("icacls.exe", args, 65536),
            system.clone(),
            options(),
        )
        .await?;
        if result.failure.is_some() {
            return Err(IvyError::internal());
        }
    }
    #[cfg(not(windows))]
    {
        let _ = job_launcher;
        tokio::task::spawn_blocking(move || std::fs::File::open(parent)?.sync_all())
            .await
            .map_err(|_| IvyError::internal())?
            .map_err(|_| IvyError::internal())?;
    }
    Ok(())
}
