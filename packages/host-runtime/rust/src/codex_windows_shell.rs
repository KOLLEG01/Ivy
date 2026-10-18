//! Unpackaged PowerShell probe; path identity is checked by the launch owner.
use super::process::{self, Command, LaunchContext, ProcessResult};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::parse};
use std::{collections::BTreeMap, path::Path};

const PROBE: &str = r#"
$ErrorActionPreference='Stop'
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class IvyNativeShellProbe { [DllImport("kernel32.dll", CharSet=CharSet.Unicode)] public static extern int GetCurrentPackageFullName(ref uint length, IntPtr name); }'
[uint32]$ivyPackageLength=0
$ivyPackageStatus=[IvyNativeShellProbe]::GetCurrentPackageFullName([ref]$ivyPackageLength,[IntPtr]::Zero)
@{packageStatus=$ivyPackageStatus;version=$PSVersionTable.PSVersion.ToString();executable=[System.Diagnostics.Process]::GetCurrentProcess().MainModule.FileName}|ConvertTo-Json -Compress
"#;

pub(crate) fn probe_command() -> Command {
    Command {
        executable: "shell".into(),
        args: [
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            PROBE,
        ]
        .into_iter()
        .map(Into::into)
        .collect(),
        cwd: None,
        timeout_ms: 15_000,
    }
}

// Exact ASCII grammar /^(\d+)\.(\d+)\.(\d+)(?:[-+][0-9A-Za-z.-]+)?$/.
fn version_matches(value: &JsString) -> bool {
    let units = &value.0;
    let mut end = 0;
    for part in 0..3 {
        let start = end;
        while units.get(end).is_some_and(|c| (48..=57).contains(c)) {
            end += 1;
        }
        if start == end {
            return false;
        }
        if part < 2 {
            if units.get(end) != Some(&46) {
                return false;
            }
            end += 1;
        }
    }
    if end == units.len() {
        return true;
    }
    if !matches!(units[end], 43 | 45) {
        return false;
    }
    end += 1;
    end < units.len()
        && units[end..]
            .iter()
            .all(|c| matches!(c, 45 | 46 | 48..=57 | 65..=90 | 97..=122))
}

/// Observed strings only: this is not a verified WindowsShellStatus. The owner
/// must compare resolved path identities and validate that final status schema.
pub(crate) struct Observation {
    pub version: JsString,
    pub executable: JsString,
}

/// The original boundary inspects stdout even when the command exits nonzero.
pub(crate) fn probe_observation(result: &ProcessResult) -> IvyResult<Observation> {
    let value = parse(&result.stdout, usize::MAX).unwrap_or(IvyJson::Null);
    if value.as_object().is_none() {
        return Err(IvyError::new(
            "native_shell_mismatch",
            "Native shell identity probe returned no valid observation.",
        ));
    }
    if value.get("packageStatus") != Some(&IvyJson::Number(15700.0)) {
        return Err(IvyError::new(
            "native_shell_packaged",
            "Packaged PowerShell is outside the supported native process lifetime boundary; configure an unpackaged distribution.",
        ));
    }
    let version = value.get("version").and_then(IvyJson::as_js_string);
    let executable = value.get("executable").and_then(IvyJson::as_js_string);
    match (version, executable) {
        (Some(version), Some(executable)) if version_matches(version) => Ok(Observation {
            version: version.clone(),
            executable: executable.clone(),
        }),
        _ => Err(IvyError::new(
            "native_shell_mismatch",
            "The launched shell does not match its configured identity.",
        )),
    }
}

/// Invoke only after the owner verifies platform, settings, direct executable
/// and digest. This command retains Host job ownership and process errors.
pub(crate) async fn probe_shell(
    executable: &Path,
    cwd: &Path,
    job_launcher: &Path,
) -> IvyResult<Observation> {
    let executables = BTreeMap::from([("shell".into(), executable.to_path_buf())]);
    let result = process::run_command(
        &probe_command(),
        cwd,
        &executables,
        LaunchContext {
            job_launcher: Some(job_launcher.to_path_buf()),
            ..Default::default()
        },
        None,
    )
    .await?;
    probe_observation(&result)
}
