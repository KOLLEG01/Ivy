//! Unpackaged PowerShell probe; path identity is checked by the launch owner.
use super::{
    codex_daemon::file_hash,
    node_path, paths,
    process::{self, Command, LaunchContext, ProcessResult},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, agent_schema::validate_agent, json::parse,
};
use std::{collections::BTreeMap, ffi::OsString, path::Path};

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

/// Validate the explicit unpackaged shell before any native owner is launched.
pub async fn verify_windows_shell(
    settings: Option<&IvyJson>,
    cwd: &Path,
    job_launcher: &Path,
    context: &node_path::Context,
) -> IvyResult<IvyJson> {
    let require = |test, code, message| {
        if test {
            Ok(())
        } else {
            Err(IvyError::new(code, message))
        }
    };
    require(
        cfg!(windows)
            && settings.is_some_and(|value| match value {
                IvyJson::Null | IvyJson::Bool(false) => false,
                IvyJson::Number(number) => *number != 0.0 && !number.is_nan(),
                IvyJson::String(value) => !value.0.is_empty(),
                _ => true,
            }),
        "native_shell_required",
        "Windows native execution requires an explicitly configured unpackaged PowerShell.",
    )?;
    let settings = settings.unwrap();
    validate_agent("WindowsShell", settings)?;
    let configured = settings.required("executable")?.as_js_string().unwrap();
    require(
        node_path::is_absolute(context.style, configured),
        "native_shell_mismatch",
        "The native shell executable must be absolute.",
    )?;
    let path = node_path::to_native(configured);
    let executable = tokio::task::spawn_blocking(move || paths::realpath(&path))
        .await
        .map_err(|_| IvyError::internal())?
        .map_err(|_| IvyError::internal())?;
    let text = node_path::from_native(&executable);
    require(
        node_path::basename(context.style, &text).to_english_lowercase()
            == JsString::from("pwsh.exe")
            && tokio::fs::metadata(&executable)
                .await
                .map_err(|_| IvyError::internal())?
                .is_file(),
        "native_shell_mismatch",
        "The configured native shell must be a direct pwsh.exe executable.",
    )?;
    require(
        settings.get("executableHash") == Some(&IvyJson::from(file_hash(&executable).await?)),
        "native_shell_mismatch",
        "Native shell bytes differ from the configured digest.",
    )?;
    let observed = probe_shell(&executable, cwd, job_launcher).await?;
    let folded = |value: &JsString| context.resolve(&[value.clone()]).to_english_lowercase();
    require(
        folded(&observed.executable) == folded(&text),
        "native_shell_mismatch",
        "The launched shell does not match its configured identity.",
    )?;
    let status = IvyJson::object([
        ("executable", IvyJson::String(text)),
        (
            "executableHash",
            settings.required("executableHash")?.clone(),
        ),
        ("version", IvyJson::String(observed.version)),
        ("packageIdentity", "unpackaged".into()),
    ]);
    validate_agent("WindowsShellStatus", &status)?;
    Ok(status)
}

pub fn windows_shell_environment(
    shell: &IvyJson,
    environment: &BTreeMap<JsString, JsString>,
) -> IvyResult<BTreeMap<OsString, OsString>> {
    let inherited = environment
        .iter()
        .find(|(key, _)| key.to_english_lowercase() == JsString::from("path"))
        .map(|(_, value)| value.0.as_slice())
        .unwrap_or(&[]);
    let executable = shell
        .required("executable")?
        .as_js_string()
        .ok_or_else(IvyError::internal)?;
    let mut path = node_path::dirname(node_path::Style::Windows, executable).0;
    if !inherited.is_empty() {
        path.push(59);
        path.extend_from_slice(inherited);
    }
    Ok(BTreeMap::from([(
        "PATH".into(),
        node_path::to_native(&JsString(path)).into_os_string(),
    )]))
}
