use super::*;
use crate::{
    local_journal::LocalJournal, local_retention::HistoryLimits, native_journal::NativeJournal,
};
use ivy_sdk::service::{ServiceClient, ServiceOptions};
use records::{load, save};
use std::{collections::HashMap, fs, sync::Mutex, time::Duration};

pub fn require_stopped(pid: u32) -> IvyResult<()> {
    require(
        pid > 0 && pid != std::process::id(),
        "invalid_arguments",
        "Name the stopped TaskBoard process with --stopped-pid.",
    )?;
    #[cfg(windows)]
    let exists = ivy_host_runtime::platform::windows_process::process_identity(pid)
        .map_err(|_| {
            IvyError::new(
                "access_denied",
                "The named TaskBoard process cannot be inspected.",
            )
        })?
        .is_some();
    #[cfg(not(windows))]
    let exists = match fs::metadata(format!("/proc/{pid}")) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => {
            return Err(IvyError::new(
                "access_denied",
                "The named TaskBoard process cannot be inspected.",
            ));
        }
    };
    require(
        !exists,
        "task_board_owner_running",
        "Stop the owning TaskBoard instance before maintenance.",
    )
}
async fn verified_file(path: &str, expected: &str) -> IvyResult<()> {
    require(
        records::absolute(path) && records::hash(expected),
        "invalid_arguments",
        "A verified export requires an absolute path and SHA-256 hash.",
    )?;
    let path = PathBuf::from(path);
    let expected = expected.to_owned();
    tokio::task::spawn_blocking(move || {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let mut file = fs::File::open(path).map_err(|_| {
            IvyError::new(
                "task_board_evidence_mismatch",
                "The verified export is unavailable.",
            )
        })?;
        let mut hash = Sha256::new();
        let mut bytes = [0u8; 65536];
        loop {
            let count = file.read(&mut bytes).map_err(|_| {
                IvyError::new("storage_unavailable", "The verified export cannot be read.")
            })?;
            if count == 0 {
                break;
            }
            hash.update(&bytes[..count]);
        }
        let observed = hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        require(
            format!("sha256:{observed}") == expected,
            "task_board_evidence_mismatch",
            "The local export differs from its verified hash.",
        )
    })
    .await
    .map_err(|_| IvyError::internal())?
}

/// The operator registers the same native owner while normal handlers remain
/// unavailable. It writes durable progress, then leaves collection to startup.
pub async fn run(
    config_path: PathBuf,
    plan_path: PathBuf,
    stopped_pid: u32,
    comments: bool,
) -> IvyResult<Json> {
    require_stopped(stopped_pid)?;
    require(
        config_path.is_absolute() && plan_path.is_absolute(),
        "invalid_arguments",
        "Use absolute configuration and plan paths.",
    )?;
    let config = ivy_host_runtime::config::native_instance_config(config_path).await?;
    let value = config.json();
    require(
        value.get("componentId") == Some(&"task-board".into()),
        "invalid_arguments",
        "Use the owning TaskBoard instance configuration.",
    )?;
    let context = MaintenanceContext::from_config(&config)?;
    let contracts = crate::runtime_inputs::contracts()?;
    contracts.validate_definition("Settings", value.required("settings")?)?;
    let registry = crate::runtime_inputs::registry(&contracts)?;
    let artifact: Option<ArtifactPlan> = if comments {
        None
    } else {
        Some(
            load(plan_path.clone())
                .await?
                .ok_or_else(IvyError::internal)?,
        )
    };
    let comment: Option<CommentJob> = if comments {
        Some(load(plan_path).await?.ok_or_else(IvyError::internal)?)
    } else {
        None
    };
    if let Some(plan) = &artifact {
        context.validate_plan(plan)?;
    }
    if let Some(job) = &comment {
        context.manifest(job.schema_version, &job.scope, &job.archive_manifest)?;
    }
    let manifest = artifact
        .as_ref()
        .map(|p| &p.archive_manifest)
        .or_else(|| comment.as_ref().map(|p| &p.archive_manifest))
        .unwrap();
    verified_file(&manifest.path, &manifest.content_hash).await?;
    if let Some(plan) = &artifact {
        for file in &plan.files {
            verified_file(&file.archive.local_path, &file.archive.content_hash).await?;
        }
    }
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let credential = text(value, "credential")?.clone();
    let mut options = ServiceOptions::new_js(
        value.required("publicBaseUrl")?.as_str()?,
        Arc::new(move || {
            let credential = credential.clone();
            Box::pin(async move { Ok(credential) })
        }),
        Json::object([
            (
                "serviceNodeId",
                Json::String(context.service_node_id.clone()),
            ),
            ("serviceName", "task-board".into()),
            ("instanceMode", "singleton".into()),
            ("hostId", context.host_id.clone().into()),
            ("version", value.required("version")?.clone()),
            ("buildId", value.required("buildId")?.clone()),
            ("hiveProtocol", Json::Number(1.0)),
        ]),
        registry.dependency(),
        Arc::new(move |connection| {
            let sender = sender.clone();
            Box::pin(async move {
                if let Some(sender) = sender.lock().unwrap().take() {
                    let _ = sender.send(connection);
                }
                Ok(())
            })
        }),
    );
    options.heartbeat = Duration::from_secs(2);
    options.readiness = Some(Arc::new(|_| {
        Box::pin(async {
            Ok(Json::object([
                ("ready", false.into()),
                ("diagnostics", Json::Array(vec![])),
            ]))
        })
    }));
    let handler: ivy_sdk::service::ToolHandler = Arc::new(|_, _| {
        Box::pin(async {
            Err(IvyError::new(
                "service_not_ready",
                "TaskBoard storage maintenance is in progress.",
            ))
        })
    });
    options.handlers = registry
        .tool_names()
        .iter()
        .map(|name| {
            (
                format!("task-board.{}", name.to_utf8().unwrap()).into(),
                handler.clone(),
            )
        })
        .collect::<HashMap<_, _>>();
    let client = ServiceClient::new(options)?;
    client.start();
    let work = async {
        let connection = tokio::time::timeout(Duration::from_secs(30), receiver)
            .await
            .map_err(|_| {
                IvyError::new(
                    "service_unavailable",
                    "Maintenance owner connection timed out.",
                )
            })?
            .map_err(|_| IvyError::internal())?;
        require_stopped(stopped_pid)?;
        crate::inspection_journal::verify_owner(
            &connection,
            &context.principal_id,
            &context.service_node_id,
        )
        .await?;
        let journal_owner =
            NativeJournal::new(Some(context.data_root.clone()), HistoryLimits::default());
        let opened = journal_owner.open()?;
        let root = value
            .required("settings")?
            .required("rootObjectId")?
            .as_js_string()
            .cloned();
        let journal = JournalStore {
            journal: LocalJournal {
                connection: opened.connection(),
                root_object_id: root,
            },
            client: Arc::new(connection),
            contracts: &contracts,
            limits: HistoryLimits::default(),
            now: records::now as fn() -> String,
            new_id: new_id as fn() -> JsString,
            cache: None,
        };
        let maintenance = Maintenance::new(&journal, context.clone());
        let result = if let Some(plan) = artifact {
            let job = maintenance.admit_artifacts(plan).await?;
            Ok(Json::object([
                ("phase", "retirement-complete".into()),
                (
                    "pendingTasks",
                    Json::Number(job.retire_pending.len() as f64),
                ),
                ("pendingFiles", Json::Number(job.remaining.len() as f64)),
            ]))
        } else {
            let job = comment.unwrap();
            let path = context.data_root.join("comment-cleanup.json");
            if let Some(previous) = load::<CommentJob>(path.clone()).await? {
                context.manifest(
                    previous.schema_version,
                    &previous.scope,
                    &previous.archive_manifest,
                )?;
                require(
                    previous.scope == job.scope
                        && previous.archive_manifest == job.archive_manifest,
                    "mutation_conflict",
                    "Another verified comment export already owns this cleanup.",
                )?;
            } else {
                save(&path, &job).await?;
            }
            maintenance.pass().await?;
            Ok(Json::object([("phase", "comment-archive-admitted".into())]))
        };
        maintenance.retention.settled().await;
        result
    }
    .await;
    client.stop().await;
    work
}
fn new_id() -> JsString {
    uuid::Uuid::new_v4().to_string().into()
}

pub async fn command(args: Vec<String>) -> IvyResult<Json> {
    let mut values = HashMap::new();
    let mut comments = false;
    let mut args = args.into_iter();
    while let Some(key) = args.next() {
        if key == "--comments" {
            comments = true;
            continue;
        }
        require(
            ["--config", "--plan", "--stopped-pid"].contains(&key.as_str()),
            "invalid_arguments",
            "Use --config PATH --plan PATH --stopped-pid PID and optional --comments.",
        )?;
        let value = args.next().ok_or_else(|| {
            IvyError::new("invalid_arguments", "A maintenance option needs its value.")
        })?;
        require(
            values.insert(key, value).is_none(),
            "invalid_arguments",
            "Maintenance options must be unique.",
        )?;
    }
    let required = |key| {
        values.get(key).cloned().ok_or_else(|| {
            IvyError::new(
                "invalid_arguments",
                "Use --config PATH --plan PATH --stopped-pid PID.",
            )
        })
    };
    run(
        required("--config")?.into(),
        required("--plan")?.into(),
        required("--stopped-pid")?.parse().map_err(|_| {
            IvyError::new(
                "invalid_arguments",
                "The stopped process ID must be a positive integer.",
            )
        })?,
        comments,
    )
    .await
}
