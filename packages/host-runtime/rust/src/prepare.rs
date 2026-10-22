//! Candidate preparation owns its accepted build through publication and cleanup.
use crate::{
    IvyError, IvyJson, IvyResult, Outcome,
    artifact::{self, FileCompare},
    config, dependency_cache,
    emitted_runtime::{JavaScriptImports, emitted_runtime_packages},
    executor_lock::ExecutorLock,
    filesystem,
    journal::HostJournal,
    layout, node_path,
    node_runtime::NodeRuntime,
    package_archive, paths, portable_dependencies,
    portable_http::PortableHttp,
    preparation_cache::PreparationCache,
    preparation_files,
    preparation_inputs::{self, PreparationProfile},
    preparation_progress::{PreparationProgress, StepOptions},
    preparation_tools, preparation_transfer, preparations, process, runtime_dependencies, source,
    storage_space,
};
use ivy_contracts::{JsString, core_schema::validate_host, json};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

/// Mandatory production parser/locale/transport inputs; no ambient fallback.
#[derive(Clone)]
pub struct PreparationRuntime {
    pub node: NodeRuntime,
    pub compare: FileCompare,
    pub imports: JavaScriptImports,
    pub http: PortableHttp,
}
fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> {
    value.map_err(|_| IvyError::internal())
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field(value: &IvyJson, key: &str) -> IvyResult<PathBuf> {
    config::path_value(value.required(key)?)
}
fn fs_path(path: &Path) -> PathBuf {
    node_path::filesystem_path(&node_path::from_native(path))
}
fn now() -> IvyJson {
    chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        .into()
}
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> IvyResult<T> + Send + 'static,
) -> IvyResult<T> {
    internal(tokio::task::spawn_blocking(work).await)?
}
async fn mkdir(path: PathBuf) -> IvyResult<()> {
    blocking(move || internal(fs::create_dir_all(fs_path(&path)))).await
}
async fn actual(path: PathBuf) -> IvyResult<PathBuf> {
    blocking(move || internal(paths::realpath(&path))).await
}
fn profile(node: &NodeRuntime) -> PreparationProfile {
    PreparationProfile {
        os: if cfg!(windows) {
            "win32"
        } else {
            std::env::consts::OS
        }
        .into(),
        arch: match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            other => other,
        }
        .into(),
        node: if node.version.starts_with('v') {
            node.version.clone()
        } else {
            format!("v{}", node.version)
        }
        .into(),
    }
}
fn executables(config: &IvyJson) -> IvyResult<BTreeMap<String, PathBuf>> {
    json::object_entries(config.required("executables")?)
        .into_iter()
        .filter_map(|(name, value)| {
            name.to_utf8()
                .ok()
                .map(|name| Ok((name, config::path_value(value)?)))
        })
        .collect()
}
fn command(value: &IvyJson) -> IvyResult<process::Command> {
    Ok(process::Command {
        executable: String::from_utf16_lossy(
            &value
                .required("executable")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?
                .0,
        ),
        args: value
            .required("args")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .map(|value| {
                Ok(OsString::from(String::from_utf16_lossy(
                    &value.as_js_string().ok_or_else(IvyError::internal)?.0,
                )))
            })
            .collect::<IvyResult<_>>()?,
        cwd: value.get("cwd").map(config::path_value).transpose()?,
        timeout_ms: value
            .required("timeoutMs")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as u64,
    })
}
fn command_text(value: &IvyJson) -> IvyResult<JsString> {
    let mut text = value
        .required("executable")?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)?
        .0;
    for argument in value
        .required("args")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        text.push(32);
        text.extend(&argument.as_js_string().ok_or_else(IvyError::internal)?.0);
    }
    Ok(JsString(text))
}
fn manifest(plan: &IvyJson, build_id: &str) -> IvyResult<IvyJson> {
    let mut value = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("buildId", build_id.into()),
    ]);
    for key in [
        "componentId",
        "kind",
        "version",
        "connectsToHive",
        "requirements",
    ] {
        value.insert(key, plan.required(key)?.clone());
    }
    let app = plan.get("kind") == Some(&"app".into());
    if !app {
        require(
            ["entrypoint", "readiness", "shutdown", "restart"]
                .iter()
                .all(|key| plan.get(key).is_some_and(|value| !value.is_null())),
            "artifact_missing",
            "Executable release plan is incomplete.",
        )?;
    }
    for key in if app {
        &["app"][..]
    } else {
        &["entrypoint", "readiness", "shutdown", "restart"][..]
    } {
        value.insert(*key, plan.required(key)?.clone());
    }
    for key in ["storage", "runtimeReset"] {
        if let Some(item) = plan.get(key).filter(|value| !value.is_null()) {
            value.insert(key, item.clone());
        }
    }
    validate_host("ReleaseManifest", &value)?;
    Ok(value)
}

/// Admitted preparation survives a caller dropping its wait, including build
/// locks, cache leases, progress, journal publication and terminal cleanup.
pub async fn prepare_candidate(
    snapshot: IvyJson,
    component_id: JsString,
    config: IvyJson,
    bootstrap_root: PathBuf,
    runtime: PreparationRuntime,
) -> IvyResult<IvyJson> {
    internal(
        tokio::spawn(prepare(
            snapshot,
            component_id,
            config,
            bootstrap_root,
            runtime,
        ))
        .await,
    )?
}
async fn prepare(
    snapshot: IvyJson,
    component: JsString,
    config: IvyJson,
    bootstrap: PathBuf,
    runtime: PreparationRuntime,
) -> IvyResult<IvyJson> {
    preparation_inputs::validate_component_id(&component)?;
    let staging = field(&config, "stagingRoot")?;
    source::verify_snapshot_metadata(snapshot.clone(), staging.clone()).await?;
    let captured_source = field(&snapshot, "sourceRoot")?;
    let component_id = component.to_utf8()?;
    let plan = source::source_build_plan(captured_source.clone(), &component_id).await?;
    preparation_inputs::validate_preparation_plan(&component, &plan)?;
    let package_manager =
        preparation_inputs::preparation_package_manager(captured_source.clone()).await?;
    let temporary = staging.join("command-temp");
    mkdir(temporary.clone()).await?;
    require(
        internal(paths::inside(
            &actual(staging.clone()).await?,
            &actual(temporary.clone()).await?,
        ))?,
        "target_conflict",
        "Preparation temporary storage leaves staging.",
    )?;
    let tools = preparation_tools::preparation_tools(
        config.required("executables")?.clone(),
        bootstrap.clone(),
        temporary.clone(),
        runtime.node.clone(),
    )
    .await?;
    let runtime_profile = profile(&runtime.node);
    let identity = preparation_inputs::preparation_identity(
        captured_source.clone(),
        component.clone(),
        plan.required("version")?
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)?,
        package_manager,
        profile(&runtime.node),
        runtime.compare.clone(),
    )
    .await?;
    let cache_root = staging
        .join("dependency-cache")
        .join(&identity.dependency_key[7..]);
    let build_id = identity.build_id;
    let progress = PreparationProgress::new(
        &config,
        component,
        snapshot
            .required("snapshotId")?
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)?,
    )?;
    let mut lock = None;
    let mut lease = None;
    let mut journal = None;
    let mut record: Option<IvyJson> = None;
    let mut registered = None;
    let artifact_boundary = field(&config, "artifactRoot")?;
    let operation: IvyResult<IvyJson> = async {
        progress
            .step("checking-existing-candidate".into(), StepOptions::default())
            .await?;
        lock = Some(
            ExecutorLock::acquire(staging.join("build-locks").join(&build_id[7..]), false).await?,
        );
        journal = Some(HostJournal::open(config.clone()).await?);
        let journal = journal.as_ref().unwrap();
        if let Some(candidate) =
            preparations::recover_prepared_build_locked(journal, &build_id, &runtime.node).await?
        {
            artifact::verify_candidate(candidate.clone(), artifact_boundary.clone(), &runtime.node)
                .await?;
            return Ok(candidate);
        }
        let previous: IvyResult<IvyJson> = async {
            let candidate = journal
                .candidate_for_build(&component_id, &build_id)
                .await?;
            artifact::verify_candidate(candidate.clone(), artifact_boundary.clone(), &runtime.node)
                .await?;
            Ok(candidate)
        }
        .await;
        match previous {
            Ok(candidate) => return Ok(candidate),
            Err(error) if error.code == "not_found" => {}
            Err(error) => return Err(error),
        }
        mkdir(artifact_boundary.clone()).await?;
        storage_space::require_storage_space(staging.clone(), (512 * 1024 * 1024) as f64).await?;
        storage_space::require_storage_space(artifact_boundary.clone(), (256 * 1024 * 1024) as f64)
            .await?;
        let preparation_id = preparations::preparation_id_for_build(&build_id)?;
        let root = staging.join(format!("build-{preparation_id}"));
        let source_root = root.join("source");
        let artifact_root = root.join("artifact");
        mkdir(root.clone()).await?;
        filesystem::remove_force(source_root.clone()).await?;
        filesystem::remove_force(artifact_root.clone()).await?;
        let started = now();
        record = Some(IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("preparationId", preparation_id.into()),
            ("componentId", component_id.clone().into()),
            ("buildId", build_id.clone().into()),
            ("snapshot", snapshot.clone()),
            ("phase", "building".into()),
            ("candidate", IvyJson::Null),
            ("startedAt", started.clone()),
            ("updatedAt", started),
            ("errorCode", IvyJson::Null),
        ]));
        preparations::save_preparation(staging.clone(), record.as_ref().unwrap().clone()).await?;
        let mut environment = tools.environment;
        environment.insert("IVY_BUILD_ID".into(), build_id.clone().into());
        environment.insert(
            "IVY_BUILD_CACHE_ROOT".into(),
            fs_path(&staging.join("build-cache")).into_os_string(),
        );
        progress
            .step("copying-source".into(), StepOptions::default())
            .await?;
        preparation_files::copy_source_tree(captured_source.clone(), source_root.clone()).await?;
        let package_lock = config::json_file(
            source_root.join("package-lock.json"),
            config::CONFIGURATION_BYTES,
        )
        .await?;
        let links = runtime_dependencies::workspace_package_locations(&package_lock)?;
        lease = Some(
            dependency_cache::use_dependency_cache(staging.clone(), &identity.dependency_key[7..])
                .await?,
        );
        dependency_cache::touch_dependency_cache(cache_root.clone()).await?;
        let cache = PreparationCache::new(
            identity.dependency_key.clone().into(),
            links,
            runtime.compare.clone(),
        );
        let cache_hit = cache
            .restore(source_root.clone(), cache_root.clone())
            .await?;
        environment.insert(
            "IVY_DEPENDENCY_CACHE_HIT".into(),
            if cache_hit { "1" } else { "0" }.into(),
        );
        let executables = executables(&config)?;
        for (index, value) in plan
            .required("prepare")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .enumerate()
        {
            progress
                .step(
                    format!("prepare-{index}").into(),
                    StepOptions {
                        command: Some(command_text(value)?),
                        ..Default::default()
                    },
                )
                .await?;
            let command = command(value)?;
            // Object lookup precedes Node's spawn UTF-8 conversion. Preserve a
            // UTF-16 mapping key even when it aliases another native spelling.
            let requested = value
                .required("executable")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?;
            let mut mapped = executables.clone();
            if let Some((_, path)) = json::object_entries(config.required("executables")?)
                .into_iter()
                .find(|(name, _)| *name == requested)
            {
                mapped.insert(command.executable.clone(), config::path_value(path)?);
            }
            process::run_command(
                &command,
                &source_root,
                &mapped,
                process::LaunchContext {
                    environment: environment.clone(),
                    job_launcher: Some(bootstrap.join("dist/native/ivy-job.exe")),
                    on_output: Some(progress.output_sink()),
                    ..Default::default()
                },
                None,
            )
            .await?;
            if !cache_hit
                && command.executable == "node"
                && command.args.first() == Some(&OsString::from("tools/build/run-npm.mjs"))
                && command.args.get(1) == Some(&OsString::from("ci"))
            {
                cache
                    .retain(source_root.clone(), cache_root.clone())
                    .await?;
            }
        }
        let app = plan.get("kind") == Some(&"app".into());
        let packages = if app {
            Vec::new()
        } else {
            runtime_dependencies::runtime_package_locations(
                &package_lock,
                emitted_runtime_packages(source_root.join("dist"), runtime.imports.clone()).await?,
            )?
        };
        mkdir(artifact_root.clone()).await?;
        progress
            .step("collecting-runtime-files".into(), StepOptions::default())
            .await?;
        if app {
            preparation_files::collect_app_bundle(
                source_root.clone(),
                artifact_root.clone(),
                plan.required("app")?
                    .required("dist")?
                    .as_js_string()
                    .cloned()
                    .ok_or_else(IvyError::internal)?,
            )
            .await?;
        } else {
            preparation_files::collect_service_files(
                source_root.clone(),
                artifact_root.clone(),
                packages.clone(),
                cache_root.clone(),
            )
            .await?;
            portable_dependencies::materialize_portable_dependencies(
                package_lock,
                packages,
                artifact_root.clone(),
                temporary,
                executables.clone(),
                runtime.http,
            )
            .await?;
        }
        config::atomic_json(
            artifact_root.join("dist/build-info.json"),
            IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("componentId", component_id.clone().into()),
                ("version", plan.required("version")?.clone()),
                ("buildId", build_id.clone().into()),
            ]),
        )
        .await?;
        let manifest = manifest(&plan, &build_id)?;
        let destination = artifact_boundary.join("candidates").join(&build_id[7..]);
        let mut candidate = IvyJson::object([
            ("candidateId", build_id.clone().into()),
            ("componentId", component_id.clone().into()),
            ("buildId", build_id.clone().into()),
            (
                "artifactRoot",
                layout::path_json(&destination.join("artifact")),
            ),
            (
                "manifestPath",
                layout::path_json(&destination.join("component.json")),
            ),
            ("createdAt", now()),
            (
                "platform",
                IvyJson::object([
                    ("os", IvyJson::String(runtime_profile.os)),
                    ("arch", IvyJson::String(runtime_profile.arch)),
                    ("node", IvyJson::String(runtime_profile.node)),
                ]),
            ),
        ]);
        progress
            .step("publishing".into(), StepOptions::default())
            .await?;
        record
            .as_mut()
            .unwrap()
            .insert("phase", "publishing".into());
        record
            .as_mut()
            .unwrap()
            .insert("candidate", candidate.clone());
        record.as_mut().unwrap().insert("updatedAt", now());
        preparations::save_preparation(staging.clone(), record.as_ref().unwrap().clone()).await?;
        let incoming = artifact_boundary.join(format!("incoming-{}", uuid::Uuid::new_v4()));
        mkdir(incoming.clone()).await?;
        require(
            internal(paths::inside(
                &actual(staging.clone()).await?,
                &actual(root.clone()).await?,
            ))? && internal(paths::inside(
                &actual(root).await?,
                &actual(artifact_root.clone()).await?,
            ))? && internal(paths::inside(
                &actual(artifact_boundary.clone()).await?,
                &actual(incoming.clone()).await?,
            ))? && internal(paths::inside(&artifact_boundary, &destination))?,
            "target_conflict",
            "Candidate promotion leaves its configured roots.",
        )?;
        preparation_transfer::transfer_prepared_payload(
            artifact_root,
            incoming.join("artifact"),
            true,
        )
        .await?;
        config::atomic_json(incoming.join("component.json"), manifest.clone()).await?;
        mkdir(artifact_boundary.join("candidates")).await?;
        let final_root = destination.clone();
        blocking(move || internal(filesystem::rename_raw(&incoming, &final_root))).await?;
        let archive =
            package_archive::create_package_archive(candidate.clone(), executables).await?;
        candidate.insert("archivePath", layout::path_json(&archive.path));
        candidate.insert("archiveHash", archive.hash.into());
        record
            .as_mut()
            .unwrap()
            .insert("candidate", candidate.clone());
        record.as_mut().unwrap().insert("updatedAt", now());
        preparations::save_preparation(staging.clone(), record.as_ref().unwrap().clone()).await?;
        progress
            .step(
                "verifying-published-artifact".into(),
                StepOptions::default(),
            )
            .await?;
        journal.save_candidate(candidate.clone(), manifest).await?;
        registered = Some(candidate.clone());
        record.as_mut().unwrap().insert("phase", "verified".into());
        record.as_mut().unwrap().insert("updatedAt", now());
        preparations::save_preparation(staging.clone(), record.as_ref().unwrap().clone()).await?;
        progress
            .step("cleaning-staging".into(), StepOptions::default())
            .await?;
        preparations::compact_preparation_locked(
            journal,
            record.as_ref().unwrap().clone(),
            true,
            false,
            &runtime.node,
        )
        .await?;
        progress
            .step("complete".into(), StepOptions::default())
            .await?;
        Ok(candidate)
    }
    .await;
    let result = async {
        match operation {
            Ok(candidate) => Ok(candidate),
            Err(error) => {
                let code = error.code_js();
                let mut failed_step = JsString::from("failed: ").0;
                failed_step.extend(&code.0);
                progress
                    .step(JsString(failed_step), StepOptions::default())
                    .await?;
                if let Some(mut failed) = record {
                    let phase = if registered.is_some() {
                        "verified"
                    } else if error.outcome == Outcome::Unknown
                        || failed.get("phase") == Some(&"publishing".into())
                    {
                        "unknown"
                    } else {
                        "failed"
                    };
                    failed.insert("phase", phase.into());
                    failed.insert("updatedAt", now());
                    failed.insert("errorCode", IvyJson::String(code));
                    let _ = preparations::save_preparation(staging, failed).await;
                }
                match registered {
                    Some(candidate) => Ok(candidate),
                    None => Err(error.with_details(IvyJson::object([(
                        "snapshotId",
                        snapshot.required("snapshotId")?.clone(),
                    )]))),
                }
            }
        }
    }
    .await;
    progress.close().await?;
    if let Some(lease) = lease {
        lease.close().await?;
    }
    if let Some(journal) = journal {
        journal.close().await?;
    }
    if let Some(lock) = lock {
        lock.close().await?;
    }
    result
}
