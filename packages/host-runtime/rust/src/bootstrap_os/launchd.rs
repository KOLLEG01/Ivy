//! Per-user macOS bootstrap ownership. launchd owns only the host; the existing
//! native Manager topology supervises ordinary services and their process trees.
use super::BootstrapOs;
use crate::{IvyError, IvyJson as J, IvyResult, Outcome, bootstrap, config, native_path};
use futures_util::future::BoxFuture;
use ivy_contracts::hash::{digest, hash_json};
use std::{
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    process::Stdio,
    time::Duration,
};

fn require(ok: bool, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new("target_conflict", message))
    }
}
fn text(value: &J, key: &str) -> IvyResult<String> {
    value.required(key)?.as_str()
}
fn array(value: &J) -> IvyResult<&[J]> {
    value.as_array().ok_or_else(IvyError::internal)
}
fn xml(value: &str) -> IvyResult<String> {
    require(
        !value
            .chars()
            .any(|c| c < ' ' && !['\n', '\r', '\t'].contains(&c)),
        "Plist strings contain invalid control characters.",
    )?;
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}
fn label(value: &str) -> IvyResult<()> {
    require(
        value.len() == 34
            && value.starts_with("ivy-next-")
            && value.as_bytes()[21] == b'-'
            && value.as_bytes()[9..21]
                .iter()
                .chain(&value.as_bytes()[22..])
                .all(u8::is_ascii_hexdigit),
        "Invalid owned launchd label.",
    )
}
fn home() -> IvyResult<PathBuf> {
    Ok(native_path::to_native(
        &crate::native_account::os_homedir().map_err(|_| IvyError::internal())?,
    ))
}
fn domain() -> String {
    format!("gui/{}", unsafe { libc::geteuid() })
}
fn path(name: &str) -> IvyResult<PathBuf> {
    label(name)?;
    Ok(home()?
        .join("Library/LaunchAgents")
        .join(format!("{name}.plist")))
}
fn definition(content: String) -> J {
    J::object([
        ("hash", digest(content.as_bytes()).into()),
        ("content", content.into()),
    ])
}
fn check(value: &J) -> IvyResult<()> {
    require(
        value.get("hash") == Some(&digest(text(value, "content")?.as_bytes()).into()),
        "Retained launchd definition changed.",
    )
}
fn strings(values: &[String]) -> IvyResult<String> {
    values
        .iter()
        .map(|s| Ok(format!("<string>{}</string>", xml(s)?)))
        .collect()
}
/// Arguments remain an argv array, including spaces and XML metacharacters.
/// Background QoS and coalescible timers never request a wake or sleep assertion.
pub fn plist(plan: &J, process: &J) -> IvyResult<String> {
    require(
        plan.get("os") == Some(&"darwin".into()),
        "launchd requires a macOS candidate.",
    )?;
    let name = text(process, "name")?;
    label(&name)?;
    let artifact = text(process, "artifactRoot").or_else(|_| text(plan, "artifactRoot"))?;
    let executable = bootstrap::process_executable(plan, process)?.to_utf8()?;
    let mut args = vec![executable];
    args.extend(
        array(process.required("entrypoint")?.required("args")?)?
            .iter()
            .map(J::as_str)
            .collect::<IvyResult<Vec<_>>>()?,
    );
    let host = process.get("componentId") == Some(&"host-executor".into());
    args.extend([
        "--config".into(),
        text(if host { plan } else { process }, "configPath")?,
    ]);
    if host && process.get("configPath").is_some() {
        args.extend(["--instance-config".into(), text(process, "configPath")?]);
    }
    let launch = bootstrap_launch(plan, process)?;
    let logs = config::path_value(plan.required("runtimeRoot")?)?.join("logs");
    let variables = [
        ("HOME", home()?.to_string_lossy().into_owned()),
        (
            "PATH",
            "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".into(),
        ),
        ("TOKIO_WORKER_THREADS", "2".into()),
        ("IVY_LAUNCH_ID", launch),
    ]
    .into_iter()
    .map(|(k, v)| Ok(format!("<key>{k}</key><string>{}</string>", xml(&v)?)))
    .collect::<IvyResult<String>>()?;
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{name}</string>\n<key>ProgramArguments</key><array>{}</array>\n<key>WorkingDirectory</key><string>{}</string>\n<key>EnvironmentVariables</key><dict>{variables}</dict>\n<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>\n<key>ProcessType</key><string>Background</string>\n<key>LowPriorityIO</key><true/><key>LowPriorityBackgroundIO</key><true/>\n<key>ThrottleInterval</key><integer>30</integer>\n<key>ExitTimeOut</key><integer>180</integer>\n<key>Umask</key><integer>63</integer>\n<key>StandardOutPath</key><string>{}</string>\n<key>StandardErrorPath</key><string>{}</string>\n</dict></plist>\n",
        strings(&args)?,
        xml(&artifact)?,
        xml(&logs.join(format!("{name}.out.log")).to_string_lossy())?,
        xml(&logs.join(format!("{name}.err.log")).to_string_lossy())?
    ))
}
fn bootstrap_launch(plan: &J, process: &J) -> IvyResult<String> {
    crate::journal::bootstrap_launch_id_js(
        plan,
        process
            .required("instanceId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?,
    )
}
async fn command(args: &[String]) -> IvyResult<std::process::Output> {
    let mut command = tokio::process::Command::new("/bin/launchctl");
    command.args(args).stdin(Stdio::null()).kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(200), command.output())
        .await
        .map_err(|_| {
            IvyError::new(
                "bootstrap_os_unavailable",
                "launchd operation timed out; inspect its original outcome.",
            )
            .with_outcome(Outcome::Unknown)
        })?
        .map_err(|_| IvyError::internal())
}
async fn success(args: &[String]) -> IvyResult<()> {
    let output = command(args).await?;
    require(
        output.status.success(),
        &format!(
            "launchd rejected the operation: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}
async fn loaded(name: &str) -> IvyResult<bool> {
    label(name)?;
    let output = command(&["print".into(), format!("{}/{name}", domain())]).await?;
    if output.status.success() {
        return Ok(true);
    }
    let error = String::from_utf8_lossy(&output.stderr);
    if error.contains("Could not find service") || error.contains("service not found") {
        Ok(false)
    } else {
        Err(IvyError::new(
            "bootstrap_os_unavailable",
            format!("Cannot inspect launchd owner: {error}"),
        ))
    }
}
async fn enabled(name: &str) -> IvyResult<bool> {
    let output = command(&["print-disabled".into(), domain()]).await?;
    require(
        output.status.success(),
        "Cannot inspect launchd disabled state.",
    )?;
    Ok(!String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.trim() == format!("\"{name}\" => true")))
}
async fn read(name: &str) -> IvyResult<Option<String>> {
    let path = path(name)?;
    let meta = match tokio::fs::symlink_metadata(&path).await {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(IvyError::internal()),
    };
    require(
        meta.is_file()
            && !meta.file_type().is_symlink()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.mode() & 0o022 == 0
            && meta.len() <= 65536,
        "launchd plist must be a bounded, protected file owned by the executing account.",
    )?;
    tokio::fs::read_to_string(path)
        .await
        .map(Some)
        .map_err(|_| IvyError::internal())
}
async fn write(name: &str, content: &str) -> IvyResult<()> {
    let path = path(name)?;
    let content = content.to_owned();
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        std::fs::create_dir_all(path.parent().ok_or_else(IvyError::internal)?)
            .map_err(|_| IvyError::internal())?;
        let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&temporary)
                .map_err(|_| IvyError::internal())?;
            file.write_all(content.as_bytes())
                .and_then(|_| file.sync_all())
                .map_err(|_| IvyError::internal())?;
            std::fs::rename(&temporary, &path).map_err(|_| IvyError::internal())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    })
    .await
    .map_err(|_| IvyError::internal())?
}
/// Initial publication admits existing bytes only when they match this exact plan.
pub async fn install(plan: &J) -> IvyResult<()> {
    let logs = config::path_value(plan.required("runtimeRoot")?)?.join("logs");
    tokio::fs::create_dir_all(&logs)
        .await
        .map_err(|_| IvyError::internal())?;
    tokio::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(|_| IvyError::internal())?;
    for process in array(plan.required("processes")?)? {
        let name = text(process, "name")?;
        let content = plist(plan, process)?;
        require(
            name.starts_with(&format!("{}-", text(plan, "installationId")?)),
            "launchd owner belongs to another installation.",
        )?;
        match read(&name).await? {
            Some(old) => require(
                old == content,
                "Existing launchd definition differs from this immutable bootstrap.",
            )?,
            None => write(&name, &content).await?,
        }
        success(&["enable".into(), format!("{}/{name}", domain())]).await?;
        if !loaded(&name).await? {
            success(&[
                "bootstrap".into(),
                domain(),
                path(&name)?.to_string_lossy().into_owned(),
            ])
            .await?;
        }
    }
    Ok(())
}
#[derive(Clone)]
pub struct LaunchdBootstrapOs;
impl LaunchdBootstrapOs {
    async fn owner(&self, process: &J, plans: &[J]) -> IvyResult<J> {
        let name = text(process, "name")?;
        let content = read(&name)
            .await?
            .ok_or_else(|| IvyError::new("not_found", "Owned launchd definition is missing."))?;
        let mut matches = Vec::new();
        let mut hashes = std::collections::BTreeSet::new();
        for plan in plans {
            if !hashes.insert(hash_json(plan)?) {
                continue;
            }
            if let Some(p) = array(plan.required("processes")?)?
                .iter()
                .find(|p| p.get("instanceId") == process.get("instanceId"))
            {
                if plist(plan, p)? == content {
                    matches.push((plan, p));
                }
            }
        }
        require(
            matches.len() == 1,
            "launchd definition does not match one retained immutable candidate.",
        )?;
        let (plan, process) = matches[0];
        let mut owner = process.clone();
        let def = definition(content);
        owner.insert(
            "candidateId",
            process
                .get("candidateId")
                .unwrap_or(plan.required("candidateId")?)
                .clone(),
        );
        owner.insert("definitionHash", def.required("hash")?.clone());
        owner.insert("definition", def);
        owner.insert("enabled", enabled(&name).await?.into());
        owner.insert("running", loaded(&name).await?.into());
        Ok(owner)
    }
    async fn inspect_owned(&self, plans: &[J]) -> IvyResult<Vec<J>> {
        let plan = plans
            .first()
            .ok_or_else(|| IvyError::new("not_found", "No retained bootstrap plan."))?;
        let mut owners = Vec::new();
        for process in array(plan.required("processes")?)? {
            owners.push(self.owner(process, plans).await?);
        }
        Ok(owners)
    }
    async fn exact(&self, snapshot: &J, plans: &[J]) -> IvyResult<Vec<J>> {
        let first = plans.first().ok_or_else(IvyError::internal)?;
        require(
            snapshot.get("hostId") == first.get("hostId")
                && snapshot.get("installationId") == first.get("installationId"),
            "Bootstrap snapshot belongs to another installation.",
        )?;
        let owners = self.inspect_owned(plans).await?;
        let prior = array(snapshot.required("owners")?)?;
        require(
            owners.len() == prior.len()
                && owners.iter().all(|owner| {
                    prior.iter().any(|p| {
                        ["instanceId", "name", "candidateId", "definitionHash"]
                            .iter()
                            .all(|k| p.get(k) == owner.get(k))
                    })
                }),
            "launchd ownership changed after its snapshot.",
        )?;
        Ok(owners)
    }
}
impl BootstrapOs for LaunchdBootstrapOs {
    fn inspect(&self, plans: Vec<J>) -> BoxFuture<'_, IvyResult<Vec<J>>> {
        Box::pin(async move { self.inspect_owned(&plans).await })
    }
    fn render(&self, owner: J, next: J) -> BoxFuture<'_, IvyResult<J>> {
        Box::pin(async move {
            let process = array(next.required("processes")?)?
                .iter()
                .find(|p| p.get("instanceId") == owner.get("instanceId"))
                .ok_or_else(IvyError::internal)?;
            Ok(definition(plist(&next, process)?))
        })
    }
    fn check(&self, value: J) -> BoxFuture<'_, IvyResult<()>> {
        Box::pin(async move { check(&value) })
    }
    fn apply(&self, owner: J, desired: J, plans: Vec<J>) -> BoxFuture<'_, IvyResult<()>> {
        Box::pin(async move {
            check(&desired)?;
            let current = self.owner(&owner, &plans).await?;
            require(
                current.get("running") == Some(&false.into())
                    && [owner.get("definitionHash"), desired.get("hash")]
                        .contains(&current.get("definitionHash")),
                "launchd owner changed or is still loaded during maintenance.",
            )?;
            write(&text(&owner, "name")?, &text(&desired, "content")?).await
        })
    }
    fn reload(&self) -> BoxFuture<'_, IvyResult<()>> {
        Box::pin(async { Ok(()) })
    }
    fn can_pause_and_resume(&self) -> bool {
        true
    }
    fn pause(&self, snapshot: J, plans: Vec<J>) -> Option<BoxFuture<'_, IvyResult<()>>> {
        Some(Box::pin(async move {
            for owner in self.exact(&snapshot, &plans).await? {
                if owner.get("running") == Some(&true.into()) {
                    success(&[
                        "bootout".into(),
                        format!("{}/{}", domain(), text(&owner, "name")?),
                    ])
                    .await?;
                }
            }
            require(
                self.inspect_owned(&plans)
                    .await?
                    .iter()
                    .all(|o| o.get("running") == Some(&false.into())),
                "launchd owner did not stop.",
            )
        }))
    }
    fn resume(&self, snapshot: J, plans: Vec<J>) -> Option<BoxFuture<'_, IvyResult<()>>> {
        Some(Box::pin(async move {
            let owners = self.inspect_owned(&plans).await?;
            let prior = array(snapshot.required("owners")?)?;
            require(
                owners.len() == prior.len()
                    && owners.iter().all(|owner| {
                        prior.iter().any(|p| {
                            ["instanceId", "name", "enabled"]
                                .iter()
                                .all(|k| p.get(k) == owner.get(k))
                        })
                    }),
                "launchd scheduling changed during maintenance.",
            )?;
            for owner in owners {
                if let Some(p) = prior
                    .iter()
                    .find(|p| p.get("instanceId") == owner.get("instanceId"))
                {
                    if p.get("running") == Some(&true.into())
                        && owner.get("running") == Some(&false.into())
                    {
                        success(&[
                            "bootstrap".into(),
                            domain(),
                            path(&text(&owner, "name")?)?.to_string_lossy().into_owned(),
                        ])
                        .await?;
                    }
                }
            }
            Ok(())
        }))
    }
    fn launch_automatic(
        &self,
        plan: J,
        request: PathBuf,
        operation: String,
    ) -> Option<BoxFuture<'_, IvyResult<()>>> {
        Some(Box::pin(async move {
            let name = format!(
                "{}-maintenance-{}",
                text(&plan, "installationId")?,
                &digest(operation.as_bytes())[7..19]
            );
            let worker = super::automatic::worker(&plan, &request, "ivy-bootstrap-auto-worker")?;
            let logs = config::path_value(plan.required("runtimeRoot")?)?.join("logs");
            let mut args = vec![
                "submit".into(),
                "-l".into(),
                name,
                "-o".into(),
                logs.join("maintenance.out.log")
                    .to_string_lossy()
                    .into_owned(),
                "-e".into(),
                logs.join("maintenance.err.log")
                    .to_string_lossy()
                    .into_owned(),
                "--".into(),
                worker.executable.to_utf8()?,
            ];
            args.extend(
                worker
                    .args
                    .iter()
                    .map(|s| s.to_utf8())
                    .collect::<IvyResult<Vec<_>>>()?,
            );
            success(&args).await
        }))
    }
}
