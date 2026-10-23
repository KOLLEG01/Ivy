//! Verified official shared-daemon startup. The daemon never belongs to this owner.
use super::process::{self, Command, LaunchContext, ProcessResult};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::io::AsyncReadExt;

fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}

fn absolute(value: &IvyJson) -> bool {
    let Some(value) = value.as_js_string() else {
        return false;
    };
    let units = &value.0;
    #[cfg(windows)]
    {
        let separator = |c| c == 47 || c == 92;
        units.first().is_some_and(|c| separator(*c))
            || (units.len() >= 3
                && ((65..=90).contains(&units[0]) || (97..=122).contains(&units[0]))
                && units[1] == 58
                && separator(units[2]))
    }
    #[cfg(not(windows))]
    {
        units.first() == Some(&47)
    }
}

fn path(value: &JsString) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&value.0).into()
    }
    #[cfg(not(windows))]
    {
        String::from_utf16_lossy(&value.0).into()
    }
}

/// Admit the literal lifecycle status, version and native endpoint fields.
pub fn daemon_observation(value: &IvyJson, expected_version: &JsString) -> IvyResult<IvyJson> {
    require(
        value.as_object().is_some(),
        "native_daemon_response_invalid",
        "Official daemon startup did not return a lifecycle object.",
    )?;
    let status = value.get("status");
    let status_text = status.and_then(IvyJson::as_js_string);
    let pid = value.get("pid");
    require((status_text == Some(&JsString::from("started")) || status_text == Some(&JsString::from("alreadyRunning"))) &&
        value.get("socketPath").is_some_and(absolute) && value.get("managedCodexPath").is_some_and(absolute) &&
        pid.is_none_or(|value| matches!(value, IvyJson::Number(n) if n.is_finite() && *n > 0.0 && *n <= 9_007_199_254_740_991.0 && n.fract() == 0.0)),
        "native_daemon_response_invalid", "Official daemon startup returned an invalid endpoint or process identity.")?;
    require(
        ["cliVersion", "managedCodexVersion", "appServerVersion"]
            .iter()
            .all(|key| value.get(key).and_then(IvyJson::as_js_string) == Some(expected_version)),
        "native_server_version_mismatch",
        "The shared server, managed installation and selected CLI must have the configured exact version. Existing servers are not restarted automatically.",
    )?;
    Ok(IvyJson::object([
        ("status", status.unwrap().clone()),
        ("socketPath", value.get("socketPath").unwrap().clone()),
        (
            "managedCodexPath",
            value.get("managedCodexPath").unwrap().clone(),
        ),
        (
            "managedCodexVersion",
            IvyJson::String(expected_version.clone()),
        ),
        ("cliVersion", IvyJson::String(expected_version.clone())),
        (
            "appServerVersion",
            IvyJson::String(expected_version.clone()),
        ),
        ("pid", pid.cloned().unwrap_or(IvyJson::Null)),
    ]))
}

pub struct Settings {
    pub executable: PathBuf,
    pub executable_hash: String,
    pub version: JsString,
    pub home: PathBuf,
    pub job_launcher: PathBuf,
    pub environment: BTreeMap<OsString, OsString>,
}

pub(crate) async fn file_hash(path: &Path) -> IvyResult<String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| IvyError::internal())?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file
            .read(&mut buffer)
            .await
            .map_err(|_| IvyError::internal())?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let mut result = String::from("sha256:");
    for byte in hash.finalize() {
        use std::fmt::Write;
        write!(result, "{byte:02x}").unwrap();
    }
    Ok(result)
}

// Equivalent to /\/ivy-next-[^/\n]+\.service(?:\/|$)/m; no cgroup ownership mutation.
pub(crate) fn in_runtime_service(cgroups: &str) -> bool {
    cgroups.match_indices("/ivy-next-").any(|(start, _)| {
        let tail = &cgroups[start + "/ivy-next-".len()..];
        tail.match_indices(".service").any(|(end, _)| {
            end > 0
                && !tail[..end].contains(['/', '\n'])
                && tail[end + ".service".len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| matches!(c, '/' | '\n' | '\r' | '\u{2028}' | '\u{2029}'))
        })
    })
}

pub(crate) struct Invocation {
    pub command: Command,
    pub home: PathBuf,
    pub executables: BTreeMap<String, PathBuf>,
    pub context: LaunchContext,
}

pub(crate) fn invocation(settings: &Settings, user_scope: Option<(u32, &str)>) -> Invocation {
    let mut executables = BTreeMap::from([("codex".into(), settings.executable.clone())]);
    let mut environment = settings.environment.clone();
    environment.insert("CODEX_HOME".into(), settings.home.clone().into_os_string());
    let mut command = Command {
        executable: "codex".into(),
        args: ["app-server", "daemon", "start"].map(Into::into).into(),
        cwd: None,
        timeout_ms: 100_000,
    };
    if let Some((uid, unit)) = user_scope {
        let runtime = format!("/run/user/{uid}");
        executables.insert("systemd-run".into(), "/usr/bin/systemd-run".into());
        environment.insert("XDG_RUNTIME_DIR".into(), runtime.clone().into());
        environment.insert(
            "DBUS_SESSION_BUS_ADDRESS".into(),
            format!("unix:path={runtime}/bus").into(),
        );
        command.executable = "systemd-run".into();
        let mut args: Vec<OsString> = ["--user", "--scope", "--quiet"].map(Into::into).into();
        args.extend([
            format!("--unit=ivy-codex-start-{unit}").into(),
            "--".into(),
            settings.executable.clone().into_os_string(),
        ]);
        args.append(&mut command.args);
        command.args = args;
    }
    Invocation {
        command,
        home: settings.home.clone(),
        executables,
        context: LaunchContext {
            environment,
            job_launcher: Some(settings.job_launcher.clone()),
            allow_windows_breakaway: true,
            ..Default::default()
        },
    }
}

/// No updater, restart, stop, pairing, replay, or implicit owned-stdio fallback.
pub async fn ensure_shared_daemon(settings: Settings) -> IvyResult<IvyJson> {
    ensure_with(settings, |launch| async move {
        process::run_command(
            &launch.command,
            &launch.home,
            &launch.executables,
            launch.context,
            None,
        )
        .await
    })
    .await
}

// The runner seam keeps filesystem identity checks real in protocol-only tests.
pub(crate) async fn ensure_with<F, R>(settings: Settings, run: F) -> IvyResult<IvyJson>
where
    F: FnOnce(Invocation) -> R,
    R: Future<Output = IvyResult<Arc<ProcessResult>>>,
{
    let filename = if cfg!(windows) { "codex.exe" } else { "codex" };
    let current = settings.home.join("packages/standalone/current");
    let mut managed = current.join("bin").join(filename);
    match tokio::fs::metadata(&managed).await {
        Ok(metadata) => require(
            metadata.is_file(),
            "native_daemon_installation_missing",
            "The selected home needs its verified standalone Codex installation.",
        )?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            managed = current.join(filename)
        }
        Err(_) => return Err(IvyError::internal()),
    }
    let actual = tokio::fs::canonicalize(&managed).await.map_err(|_| IvyError::new("native_daemon_installation_missing", "Install the pinned standalone Codex package in the selected home before starting its shared daemon."))?;
    require(
        file_hash(&actual).await? == settings.executable_hash,
        "native_daemon_installation_mismatch",
        "The managed standalone server executable differs from the selected verified CLI.",
    )?;
    #[cfg(target_os = "linux")]
    let scope = {
        let cgroups = tokio::fs::read("/proc/self/cgroup")
            .await
            .map_err(|_| IvyError::internal())?;
        if in_runtime_service(&String::from_utf8_lossy(&cgroups)) {
            use std::os::unix::fs::FileTypeExt;
            let uid = unsafe { libc::getuid() };
            let bus = PathBuf::from(format!("/run/user/{uid}/bus"));
            require(
                tokio::fs::metadata(bus)
                    .await
                    .is_ok_and(|m| m.file_type().is_socket()),
                "native_daemon_user_bus_unavailable",
                "A running user systemd manager is required to start a shared daemon outside this Ivy service cgroup.",
            )?;
            Some((uid, uuid::Uuid::new_v4().to_string()))
        } else {
            None
        }
    };
    #[cfg(not(target_os = "linux"))]
    let scope: Option<(u32, String)> = None;
    let result = run(invocation(
        &settings,
        scope.as_ref().map(|(uid, unit)| (*uid, unit.as_str())),
    ))
    .await?;
    require(
        result.exit_code == Some(0) && result.error_code.is_none() && !result.truncated,
        "native_daemon_start_failed",
        "Official shared daemon startup failed or timed out. Reconnection may retry idempotent start; no accepted native request is replayed.",
    )?;
    let value = json::parse(&result.stdout, usize::MAX).map_err(|_| {
        IvyError::new(
            "native_daemon_response_invalid",
            "Official daemon startup did not return one JSON lifecycle response.",
        )
    })?;
    let observed = daemon_observation(&value, &settings.version)?;
    let reported = path(
        observed
            .get("managedCodexPath")
            .unwrap()
            .as_js_string()
            .unwrap(),
    );
    require(
        tokio::fs::canonicalize(reported)
            .await
            .map_err(|_| IvyError::internal())?
            == actual
            && file_hash(&actual).await? == settings.executable_hash,
        "native_daemon_installation_mismatch",
        "The lifecycle selected another standalone server installation.",
    )?;
    Ok(observed)
}
