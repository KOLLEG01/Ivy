//! Executable identity checks used by native launch after catalog preflight.
use super::{codex_daemon::file_hash, codex_handshake::whitespace, process};
use ivy_contracts::{IvyError, IvyResult, JsString};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

/// The caller validates configured absolute paths and the stop fence first.
pub async fn verify_executable(configured: &Path, expected_hash: &JsString) -> IvyResult<PathBuf> {
    let executable = tokio::fs::canonicalize(configured)
        .await
        .map_err(|_| IvyError::internal())?;
    let metadata = tokio::fs::metadata(&executable)
        .await
        .map_err(|_| IvyError::internal())?;
    let bytes = executable.as_os_str().as_encoded_bytes();
    let suffix = &bytes[bytes.len().saturating_sub(4)..];
    // A dotfile such as .CMD also matches the original whole-path suffix regex.
    let script = [b".cmd", b".bat", b".ps1"]
        .iter()
        .any(|ending| suffix.eq_ignore_ascii_case(*ending));
    if !metadata.is_file()
        || script
        || JsString::from(file_hash(&executable).await?) != *expected_hash
    {
        return Err(IvyError::new(
            "native_build_mismatch",
            "Configured Codex executable differs from its exact catalog build.",
        ));
    }
    Ok(executable)
}

pub(crate) fn version_command() -> process::Command {
    process::Command {
        executable: "codex".into(),
        args: vec!["--version".into()],
        cwd: None,
        timeout_ms: 15_000,
    }
}

/// Match String.trim's ECMAScript whitespace, not Rust's wider Unicode trim.
pub(crate) fn verify_version_result(
    result: &process::ProcessResult,
    expected_version: &JsString,
) -> IvyResult<()> {
    let units: Vec<_> = result.stdout.encode_utf16().collect();
    let start = units
        .iter()
        .position(|unit| !whitespace(*unit))
        .unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|unit| !whitespace(*unit))
        .map_or(start, |i| i + 1);
    let expected = "codex-cli "
        .encode_utf16()
        .chain(expected_version.0.iter().copied());
    if result.exit_code != Some(0) || !units[start..end].iter().copied().eq(expected) {
        return Err(IvyError::new(
            "native_build_mismatch",
            "Codex CLI version differs from the selected catalog.",
        ));
    }
    Ok(())
}

/// Shell validation and the selected home/environment precede this bounded probe.
/// Process errors retain their original code/outcome; there is no fallback or retry.
pub async fn verify_cli_version(
    executable: &Path,
    home: &Path,
    expected_version: &JsString,
    environment: BTreeMap<OsString, OsString>,
    job_launcher: &Path,
) -> IvyResult<()> {
    let executables = BTreeMap::from([("codex".into(), executable.to_path_buf())]);
    let context = process::LaunchContext {
        environment,
        job_launcher: Some(job_launcher.to_path_buf()),
        ..Default::default()
    };
    let result =
        process::run_command(&version_command(), home, &executables, context, None).await?;
    verify_version_result(&result, expected_version)
}
