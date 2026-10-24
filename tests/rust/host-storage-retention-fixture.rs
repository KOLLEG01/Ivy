use super::{FixedClock, J, NOW, text};
use ivy_contracts::{
    core_schema::validate_host,
    hash::{digest, hash_json},
    json,
};
use ivy_host_runtime::{
    bootstrap, config, filesystem, journal::HostJournal, native_path, preparations, storage_space,
};
use ivy_store::rusqlite::{Connection, params};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

const DAY: i64 = 24 * 60 * 60 * 1000;
const SNAPSHOT: &str = "00000000-0000-5000-8000-000000000002";
const PREVIOUS_SUCCESS: &str = "2000-01-01T00:00:00.000Z";

pub struct Setup {
    pub root: PathBuf,
    pub config: J,
    pub held: Option<PathBuf>,
    plan_keys: BTreeMap<String, String>,
    history_records: BTreeMap<String, Vec<u8>>,
}
fn path(value: &J) -> PathBuf {
    native_path::to_native(value.as_js_string().unwrap())
}
fn stamp(at: i64) -> J {
    chrono::DateTime::from_timestamp_millis(at)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        .into()
}
fn age(path: &Path, at: i64) {
    filesystem::set_times_ms(path, at, at).unwrap();
}
fn names(path: &Path) -> Vec<String> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return vec![],
        Err(error) => panic!("{error}"),
    };
    let mut names = entries
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    names
}
fn database(config: &J) -> Connection {
    Connection::open(path(config.required("runtimeRoot").unwrap()).join("deployments.sqlite"))
        .unwrap()
}
fn reference(config: &J, table: &str, instance: &str, candidate: &J) {
    assert!(["installed", "runtime_targets"].contains(&table));
    let mut value = J::object([(
        "candidateId",
        candidate.required("candidateId").unwrap().clone(),
    )]);
    value.insert("instanceId", instance.into());
    value.insert("buildId", candidate.get("buildId").unwrap_or(candidate.required("candidateId").unwrap()).clone());
    if table == "installed" {
        value.insert("enabled", true.into());
        value.insert("installedAt", stamp(NOW));
    } else {
        value.insert("schemaVersion", J::Number(1.0));
        value.insert("revision", "fixture-target".into());
        value.insert("desired", "running".into());
        value.insert("configPath", text(&path(config.required("runtimeRoot").unwrap()).join(format!("{instance}.json"))));
        value.insert("requestedAt", stamp(NOW));
    }
    database(config)
        .execute(
            &format!("INSERT INTO {table} VALUES (?,?)"),
            params![instance, json::stringify(&value, usize::MAX).unwrap()],
        )
        .unwrap();
}
fn command() -> J {
    J::object([
        ("executable", "node".into()),
        ("args", J::Array(vec!["dist/main.mjs".into()])),
        ("timeoutMs", J::Number(5000.0)),
    ])
}
async fn candidate(
    root: &Path,
    config: &J,
    journal: &HostJournal,
    component: &str,
    id: String,
    version: String,
    at: i64,
) -> (J, J) {
    let directory = path(config.required("artifactRoot").unwrap())
        .join("candidates")
        .join(&id[7..]);
    fs::create_dir_all(directory.join("artifact")).unwrap();
    let candidate = J::object([
        ("candidateId", id.clone().into()),
        ("buildId", id.clone().into()),
        ("componentId", component.into()),
        ("artifactRoot", text(&directory.join("artifact"))),
        ("manifestPath", text(&directory.join("component.json"))),
        ("createdAt", stamp(at)),
        (
            "platform",
            J::object([
                ("os", if cfg!(windows) { "win32" } else { "linux" }.into()),
                ("arch", "x64".into()),
                ("node", "v24.18.0".into()),
            ]),
        ),
    ]);
    let manifest = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("componentId", component.into()),
        ("kind", "native".into()),
        ("version", version.into()),
        ("buildId", id.into()),
        ("connectsToHive", false.into()),
        (
            "requirements",
            J::object([
                ("node", ">=24.18.0 <25.0.0".into()),
                ("hiveProtocol", J::Null),
                ("contracts", J::Array(vec![])),
            ]),
        ),
        ("entrypoint", command()),
        (
            "readiness",
            J::object([("timeoutMs", J::Number(5000.0)), ("command", command())]),
        ),
        ("shutdown", J::object([("timeoutMs", J::Number(1000.0))])),
        (
            "restart",
            J::object([
                ("policy", "always".into()),
                ("minimumDelayMs", J::Number(100.0)),
                ("maximumDelayMs", J::Number(500.0)),
            ]),
        ),
    ]);
    validate_host("Candidate", &candidate).unwrap();
    validate_host("ReleaseManifest", &manifest).unwrap();
    config::atomic_json(directory.join("component.json"), manifest.clone())
        .await
        .unwrap();
    journal
        .save_candidate(candidate.clone(), manifest.clone())
        .await
        .unwrap();
    age(&directory, NOW - 5 * DAY);
    assert!(directory.starts_with(root));
    (candidate, manifest)
}
fn snapshot(root: &Path, config: &J) -> J {
    let directory = path(config.required("stagingRoot").unwrap())
        .join("sources")
        .join(SNAPSHOT);
    J::object([
        ("snapshotId", SNAPSHOT.into()),
        ("sourceRoot", text(&directory.join("source"))),
        ("originalRoot", text(&root.join("checkout"))),
        ("manifestPath", text(&directory.join("snapshot.json"))),
        ("capturedAt", stamp(NOW - 5 * DAY)),
    ])
}
async fn make_snapshot(snapshot: &J) {
    fs::create_dir_all(path(snapshot.required("sourceRoot").unwrap())).unwrap();
    fs::write(
        path(snapshot.required("sourceRoot").unwrap()).join("payload"),
        b"source authority",
    )
    .unwrap();
    let manifest = path(snapshot.required("manifestPath").unwrap());
    config::atomic_json(
        &manifest,
        J::object([
            ("schemaVersion", J::Number(1.0)),
            ("snapshot", snapshot.clone()),
        ]),
    )
    .await
    .unwrap();
    age(manifest.parent().unwrap(), NOW - 5 * DAY);
}
async fn preparation(
    config: &J,
    id: &str,
    build: J,
    snapshot: &J,
    candidate: J,
    phase: &str,
    error: J,
) {
    let staging = path(config.required("stagingRoot").unwrap());
    fs::create_dir_all(staging.join(format!("build-{id}"))).unwrap();
    preparations::save_preparation(
        staging,
        J::object([
            ("schemaVersion", J::Number(1.0)),
            ("preparationId", id.into()),
            ("componentId", "fixture".into()),
            ("buildId", build),
            ("snapshot", snapshot.clone()),
            ("phase", phase.into()),
            ("candidate", candidate),
            ("startedAt", stamp(NOW - 5 * DAY)),
            ("updatedAt", stamp(NOW - 5 * DAY)),
            ("errorCode", error),
        ]),
    )
    .await
    .unwrap();
}
fn plan(config: &J, root: &Path, candidate: &J, components: &[J]) -> J {
    let mut plan = bootstrap::configuration_bootstrap_plan(
        config,
        &native_path::from_native(&root.join("config.json")),
        candidate,
        None,
        components,
    )
    .unwrap();
    let mut processes = plan
        .required("processes")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    for process in &mut processes {
        if process.get("candidateId").is_none() {
            process.insert(
                "candidateId",
                candidate.required("candidateId").unwrap().clone(),
            );
            process.insert(
                "artifactRoot",
                candidate.required("artifactRoot").unwrap().clone(),
            );
            process.insert("entrypoint", command());
            let name = format!(
                "{}.json",
                process.required("instanceId").unwrap().as_str().unwrap()
            );
            process.insert(
                "configPath",
                text(
                    &path(config.required("runtimeRoot").unwrap())
                        .join("bootstrap")
                        .join(plan.required("installationId").unwrap().as_str().unwrap())
                        .join("instances")
                        .join(name),
                ),
            );
        }
    }
    plan.insert("processes", J::Array(processes));
    validate_host("BootstrapPlan", &plan).unwrap();
    plan
}
async fn retain(config: &J, plan: &J, at: i64) {
    let file = path(config.required("runtimeRoot").unwrap())
        .join("bootstrap-plans")
        .join(format!("{}.json", &hash_json(plan).unwrap()[7..]));
    config::atomic_json(&file, plan.clone()).await.unwrap();
    age(&file, at);
}
async fn select(config: &J, plan: &J) {
    config::atomic_json(
        path(config.required("runtimeRoot").unwrap())
            .join("bootstrap")
            .join(format!(
                "{}.json",
                plan.required("installationId").unwrap().as_str().unwrap()
            )),
        plan.clone(),
    )
    .await
    .unwrap();
    reference(config, "installed", "host-executor", plan);
}
fn request(config: &J, plans: &[J], id: &str, next: usize) -> J {
    let prior = &plans[0];
    let owners = prior
        .required("processes")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|owner| {
            J::object([
                ("instanceId", owner.required("instanceId").unwrap().clone()),
                (
                    "componentId",
                    owner.required("componentId").unwrap().clone(),
                ),
                ("name", owner.required("name").unwrap().clone()),
                (
                    "candidateId",
                    owner.required("candidateId").unwrap().clone(),
                ),
                ("definitionHash", hash_json(owner).unwrap().into()),
                ("enabled", true.into()),
                ("running", false.into()),
            ])
        })
        .collect();
    let request = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("operationId", id.into()),
        ("next", plans[next].clone()),
        (
            "previous",
            J::object([
                ("schemaVersion", J::Number(1.0)),
                ("hostId", config.required("hostId").unwrap().clone()),
                (
                    "installationId",
                    prior.required("installationId").unwrap().clone(),
                ),
                ("configPath", prior.required("configPath").unwrap().clone()),
                (
                    "configurationHash",
                    prior.required("configHash").unwrap().clone(),
                ),
                ("observedAt", stamp(NOW)),
                ("owners", J::Array(owners)),
            ]),
        ),
    ]);
    validate_host("BootstrapMaintenanceRequest", &request).unwrap();
    request
}
fn history_directory(config: &J, id: &str) -> PathBuf {
    path(config.required("runtimeRoot").unwrap())
        .join("bootstrap-maintenance")
        .join(&hash_json(&J::from(id)).unwrap()[7..])
}

pub async fn setup(root: &Path, case: &str) -> Setup {
    // Keep SQLite roots Unicode and retain the native long-path prefix.
    let root = root.join("native ü 空 😀; '");
    fs::create_dir_all(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let root = root.as_path();
    let artifact_root = root.join("artifacts ü 空 😀; '");
    #[cfg(windows)]
    let artifact_root = {
        use std::os::windows::ffi::OsStringExt;
        let mut name = artifact_root.into_os_string();
        name.push(std::ffi::OsString::from_wide(&[0xd800]));
        PathBuf::from(name)
    };
    let bootstrap_case = case.starts_with("bootstrap") || case == "maintenance-ring";
    let component = if bootstrap_case {
        "host-executor"
    } else {
        "fixture"
    };
    let executables = if case == "basic" {
        J::object([("native-host", text(&std::env::current_exe().unwrap()))])
    } else {
        let node = PathBuf::from(
            std::env::var_os("IVY_TEST_NODE")
                .expect("Pinned Node path required for the retained workload fixture"),
        );
        J::object([("node", text(&node))])
    };
    let mut config = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("hostId", "retention-fixture".into()),
        ("runtimeRoot", text(&root.join("runtime"))),
        ("artifactRoot", text(&artifact_root)),
        ("stagingRoot", text(&root.join("staging"))),
        ("publicBaseUrl", "http://127.0.0.1:38081/ivy".into()),
        ("executables", executables),
        (
            "instances",
            J::Array(vec![J::object([
                ("instanceId", component.into()),
                ("serviceNodeId", component.into()),
                ("componentId", component.into()),
                ("enabled", true.into()),
                ("engine", "process".into()),
                ("settings", J::object([] as [(&str, J); 0])),
            ])]),
        ),
    ]);
    for key in ["runtimeRoot", "artifactRoot", "stagingRoot"] {
        fs::create_dir_all(path(config.required(key).unwrap())).unwrap();
    }
    let journal = HostJournal::open_with_clock(config.clone(), Arc::new(FixedClock))
        .await
        .unwrap();
    let mut candidates = vec![];
    for number in 1..=6 {
        candidates.push(
            candidate(
                root,
                &config,
                &journal,
                component,
                format!("sha256:{}", format!("{number:x}").repeat(64)),
                format!("1.0.{}", number.min(5)),
                NOW - 5 * DAY + number * 1000,
            )
            .await
            .0,
        );
    }
    let mut plans = vec![];
    if bootstrap_case {
        for candidate in &candidates {
            let plan = plan(&config, root, candidate, &[]);
            retain(&config, &plan, NOW - 5 * DAY).await;
            plans.push(plan);
        }
    }
    let snapshot = snapshot(root, &config);
    let staging = path(config.required("stagingRoot").unwrap());
    let runtime = path(config.required("runtimeRoot").unwrap());
    let mut held = None;
    match case {
        "basic" => {
            reference(&config, "installed", "installed-fixture", &candidates[0]);
            reference(&config, "runtime_targets", "target-fixture", &candidates[2]);
            make_snapshot(&snapshot).await;
            preparation(
                &config,
                SNAPSHOT,
                candidates[1].required("buildId").unwrap().clone(),
                &snapshot,
                candidates[1].clone(),
                "compacted",
                J::Null,
            )
            .await;
        }
        "unknown-preparation" => {
            make_snapshot(&snapshot).await;
            let directory = staging.join(format!(
                "build-{}",
                preparations::preparation_id_for_build(
                    &candidates[1].required("buildId").unwrap().as_str().unwrap()
                )
                .unwrap()
            ));
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("preparation.json"), b"{bad").unwrap();
        }
        "retired-staging" | "retired-link" => {
            let retired = [
                staging.join(format!(".retiring-build-{SNAPSHOT}")),
                staging
                    .join("sources")
                    .join(format!(".retiring-{SNAPSHOT}")),
            ];
            if case == "retired-staging" {
                for directory in retired {
                    fs::create_dir_all(&directory).unwrap();
                    fs::write(directory.join("leftover"), b"partial").unwrap();
                }
            } else {
                let kept = root.join("kept");
                fs::create_dir_all(&kept).unwrap();
                fs::write(kept.join("sentinel"), b"must remain").unwrap();
                fs::create_dir_all(staging.join("sources")).unwrap();
                for directory in retired {
                    filesystem::directory_link(&kept, &directory).unwrap();
                }
            }
        }
        "pending-retirement" | "pending-unowned" => {
            let id = candidates[1]
                .required("candidateId")
                .unwrap()
                .as_str()
                .unwrap();
            assert!(journal.retire_candidate(&id).await.unwrap());
            if case == "pending-unowned" {
                let mut candidate = candidates[1].clone();
                candidate.insert("artifactRoot", text(&root.join("unowned").join("artifact")));
                database(&config)
                    .execute(
                        "UPDATE candidate_retirements SET candidate_json=? WHERE candidate_id=?",
                        params![json::stringify(&candidate, usize::MAX).unwrap(), id],
                    )
                    .unwrap();
            }
        }
        "busy-candidate" => {
            held = Some(
                staging.join("build-locks").join(
                    &candidates[3]
                        .required("candidateId")
                        .unwrap()
                        .as_str()
                        .unwrap()[7..],
                ),
            )
        }
        "busy-bootstrap" => held = Some(runtime.join("bootstrap-maintenance-lock")),
        "unknown-version" => {
            let id = candidates[1]
                .required("candidateId")
                .unwrap()
                .as_str()
                .unwrap();
            let mut manifest = journal.manifest(&id).await.unwrap();
            manifest.insert("version", "future-version".into());
            database(&config)
                .execute(
                    "UPDATE candidates SET manifest_json=? WHERE candidate_id=?",
                    params![json::stringify(&manifest, usize::MAX).unwrap(), id],
                )
                .unwrap();
        }
        "completed-unknown"
        | "unresolved-unknown"
        | "completed-scalar-request"
        | "unresolved-scalar-request" => {
            let mut record = J::object([(
                "phase",
                if case.starts_with("completed") {
                    "succeeded"
                } else {
                    "updating"
                }
                .into(),
            )]);
            if case.ends_with("scalar-request") {
                record.insert("request", false.into());
            }
            config::atomic_json(
                history_directory(&config, "unknown-history").join("record.json"),
                record,
            )
            .await
            .unwrap();
        }
        "reason-cap" => {
            for number in 1..=80 {
                let build = format!("sha256:{}", format!("{number:02x}").repeat(32));
                let id = preparations::preparation_id_for_build(&build).unwrap();
                preparation(
                    &config,
                    &id,
                    build.into(),
                    &snapshot,
                    J::Null,
                    "failed",
                    format!("fixture-error-{number}").into(),
                )
                .await;
            }
        }
        "bootstrap-order" => {
            let (extra, manifest) = candidate(
                root,
                &config,
                &journal,
                "service-manager",
                format!("sha256:{}", "0".repeat(64)),
                "1.0.1".into(),
                NOW - 5 * DAY,
            )
            .await;
            let mut instances = config
                .required("instances")
                .unwrap()
                .as_array()
                .unwrap()
                .to_vec();
            instances.push(J::object([
                ("instanceId", "service-manager".into()),
                ("componentId", "service-manager".into()),
                ("serviceNodeId", "service-manager".into()),
                ("engine", "process".into()),
                ("enabled", true.into()),
                ("settings", J::object([] as [(&str, J); 0])),
            ]));
            config.insert("instances", J::Array(instances));
            let selected = plan(&config, root, &candidates[5], &[]);
            let covering = plan(
                &config,
                root,
                &candidates[4],
                &[J::object([
                    ("candidate", extra.clone()),
                    ("manifest", manifest),
                ])],
            );
            let individual = plan(&config, root, &extra, &[]);
            for (number, plan) in [selected.clone(), covering, individual]
                .into_iter()
                .enumerate()
            {
                retain(&config, &plan, NOW - 5 * DAY + 5000 + number as i64 * 1000).await;
                plans.push(plan);
            }
            select(&config, &selected).await;
            reference(&config, "runtime_targets", "x-ref", &candidates[4]);
            reference(&config, "runtime_targets", "y-ref", &extra);
        }
        "bootstrap-current"
        | "bootstrap-prior"
        | "bootstrap-missing-time"
        | "bootstrap-null-time"
        | "bootstrap-scalar-time" => {
            select(
                &config,
                &plans[if case == "bootstrap-current" { 2 } else { 3 }],
            )
            .await;
            if case != "bootstrap-current" {
                let request = request(&config, &plans, "retention-maintenance-fixture", 3);
                let mut record = J::object([
                    ("phase", "needs_attention".into()),
                    ("updatedAt", stamp(NOW)),
                    ("request", request),
                ]);
                match case {
                    "bootstrap-missing-time" => {
                        record.remove("updatedAt");
                    }
                    "bootstrap-null-time" => record.insert("updatedAt", J::Null),
                    "bootstrap-scalar-time" => record.insert("updatedAt", false.into()),
                    _ => {}
                }
                config::atomic_json(
                    history_directory(&config, "retention-maintenance-fixture").join("record.json"),
                    record,
                )
                .await
                .unwrap();
            }
        }
        "maintenance-ring" => {
            select(&config, &plans[3]).await;
            for number in 0..25 {
                let id = format!("old-maintenance-{number}");
                let request = request(&config, &plans, &id, if number == 0 { 3 } else { 5 });
                let at = stamp(NOW - 60000 + number * 1000);
                let record = J::object([
                    ("schemaVersion", J::Number(1.0)),
                    ("request", request.clone()),
                    ("operationId", id.clone().into()),
                    ("requestHash", hash_json(&request).unwrap().into()),
                    ("createdAt", at.clone()),
                    ("updatedAt", at),
                    ("phase", "succeeded".into()),
                    ("errorCode", J::Null),
                    (
                        "definitions",
                        J::Array(vec![J::object([
                            ("instanceId", "host-executor".into()),
                            (
                                "name",
                                request
                                    .required("next")
                                    .unwrap()
                                    .required("processes")
                                    .unwrap()
                                    .as_array()
                                    .unwrap()[0]
                                    .required("name")
                                    .unwrap()
                                    .clone(),
                            ),
                            ("previousHash", digest(b"prior").into()),
                            ("desiredHash", digest(b"desired").into()),
                            ("applied", true.into()),
                        ])]),
                    ),
                ]);
                validate_host("BootstrapMaintenanceRecord", &record).unwrap();
                let directory = history_directory(&config, &id);
                config::atomic_json(directory.join("record.json"), record)
                    .await
                    .unwrap();
                for suffix in ["prior", "desired"] {
                    config::atomic_json(
                        directory.join(format!("host-executor.{suffix}.json")),
                        J::object([("content", suffix.into())]),
                    )
                    .await
                    .unwrap();
                }
            }
        }
        "active-snapshot" | "scalar-snapshot" => {
            make_snapshot(&snapshot).await;
            let mut entry = journal
                .accept(
                    J::object([
                        ("action", "deploy".into()),
                        ("operationId", "active-source".into()),
                        ("instanceId", "fixture".into()),
                        ("source", snapshot.required("originalRoot").unwrap().clone()),
                    ]),
                    Some(snapshot.clone()),
                )
                .await
                .unwrap();
            if case == "scalar-snapshot" {
                entry.insert("sourceSnapshot", false.into());
                database(&config)
                    .execute(
                        "UPDATE operations SET entry_json=? WHERE operation_id='active-source'",
                        [json::stringify(&entry, usize::MAX).unwrap()],
                    )
                    .unwrap();
            }
        }
        _ => panic!("Unknown native retention fixture: {case}"),
    }
    let mut previous = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("hostId", config.required("hostId").unwrap().clone()),
        ("state", "succeeded".into()),
        ("trigger", "manual".into()),
        ("startedAt", PREVIOUS_SUCCESS.into()),
        ("completedAt", PREVIOUS_SUCCESS.into()),
        ("lastSuccessAt", PREVIOUS_SUCCESS.into()),
        (
            "removed",
            J::object(
                [
                    "candidates",
                    "preparations",
                    "payloads",
                    "snapshots",
                    "maintenance",
                    "bootstrapPlans",
                ]
                .map(|key| (key, J::Number(0.0))),
            ),
        ),
        ("skipped", J::Array(vec![])),
        ("errorCode", J::Null),
    ]);
    for (key, value) in storage_space::host_storage_space(config.clone())
        .await
        .unwrap()
        .as_object()
        .unwrap()
    {
        previous.insert(key.to_utf8().unwrap(), value.clone());
    }
    journal
        .save_storage_retention_status(previous)
        .await
        .unwrap();
    journal.close().await.unwrap();
    let history_records = names(&runtime.join("bootstrap-maintenance"))
        .into_iter()
        .map(|name| {
            let bytes = fs::read(
                runtime
                    .join("bootstrap-maintenance")
                    .join(&name)
                    .join("record.json"),
            )
            .unwrap();
            (name, bytes)
        })
        .collect();
    let plan_keys = plans
        .iter()
        .enumerate()
        .map(|(number, plan)| {
            (
                format!("{}.json", &hash_json(plan).unwrap()[7..]),
                format!("plan-{number}"),
            )
        })
        .collect();
    Setup {
        root: root.to_owned(),
        config,
        held,
        plan_keys,
        history_records,
    }
}

pub async fn observe(setup: &Setup) -> J {
    let config = &setup.config;
    let runtime = path(config.required("runtimeRoot").unwrap());
    let staging = path(config.required("stagingRoot").unwrap());
    let journal = HostJournal::open(config.clone()).await.unwrap();
    let status = journal.storage_retention_status().await.unwrap().unwrap();
    validate_host("StorageRetentionStatus", &status).unwrap();
    let pending = journal.pending_candidate_retirements().await.unwrap();
    journal.close().await.unwrap();
    let db = database(config);
    let mut query = db
        .prepare("SELECT candidate_id FROM candidates ORDER BY candidate_id")
        .unwrap();
    let registered = query
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|row| J::from(row.unwrap()))
        .collect();
    let details = names(&runtime.join("bootstrap-maintenance"))
        .into_iter()
        .map(|name| {
            let directory = runtime.join("bootstrap-maintenance").join(&name);
            let unchanged = setup.history_records.get(&name)
                == Some(&fs::read(directory.join("record.json")).unwrap());
            J::object([
                ("name", name.into()),
                (
                    "files",
                    J::Array(names(&directory).into_iter().map(J::from).collect()),
                ),
                ("recordUnchanged", unchanged.into()),
            ])
        })
        .collect();
    let mut plans = names(&runtime.join("bootstrap-plans"))
        .into_iter()
        .map(|name| setup.plan_keys.get(&name).cloned().unwrap_or(name))
        .collect::<Vec<_>>();
    plans.sort();
    let mut observed = J::object([
        ("status", status),
        ("registered", J::Array(registered)),
        ("pending", J::Array(pending)),
        (
            "candidateDirectories",
            J::Array(
                names(&path(config.required("artifactRoot").unwrap()).join("candidates"))
                    .into_iter()
                    .map(J::from)
                    .collect(),
            ),
        ),
        (
            "preparations",
            J::Array(
                names(&staging)
                    .into_iter()
                    .filter(|name| {
                        name.strip_prefix("build-")
                            .or_else(|| name.strip_prefix(".retiring-build-"))
                            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
                    })
                    .map(J::from)
                    .collect(),
            ),
        ),
        (
            "snapshots",
            J::Array(
                names(&staging.join("sources"))
                    .into_iter()
                    .map(J::from)
                    .collect(),
            ),
        ),
        ("plans", J::Array(plans.into_iter().map(J::from).collect())),
        ("details", J::Array(details)),
    ]);
    let source = snapshot(&setup.root, config);
    let manifest = path(source.required("manifestPath").unwrap());
    let unchanged = fs::read(manifest)
        .ok()
        .and_then(|bytes| json::parse(std::str::from_utf8(&bytes).ok()?, usize::MAX).ok())
        .is_some_and(|value| {
            hash_json(&value).unwrap()
                == hash_json(&J::object([
                    ("schemaVersion", J::Number(1.0)),
                    ("snapshot", source.clone()),
                ]))
                .unwrap()
        })
        && fs::read(path(source.required("sourceRoot").unwrap()).join("payload"))
            .ok()
            .as_deref()
            == Some(b"source authority");
    observed.insert("snapshotUnchanged", unchanged.into());
    if setup.root.join("kept").is_dir() {
        observed.insert(
            "kept",
            fs::read_to_string(setup.root.join("kept/sentinel"))
                .unwrap()
                .into(),
        );
    }
    observed
}
