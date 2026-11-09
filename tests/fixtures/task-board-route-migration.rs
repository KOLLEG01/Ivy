//! Real owner transport and retained publication boundaries for a route move.
use super::*;
use ivy_contracts::native_backend::{BackendIdentity, NativeProvider};
use ivy_sdk::{
    native_parameters::save_native_parameters,
    native_plan::{NativePlanKind, read_native_plan, save_native_plan},
};
use ivy_task_board::journal_store::Contracts;
use ivy_task_board::{
    evidence_bytes::{BlobStore, ChunkKey},
    native_evidence::NativeEvidence,
    native_intent::EvidenceIdentity,
    read_evidence::ReadEvidenceStore,
};

pub(super) async fn retained_operation(
    world: &World<'_>,
    catalogs: &CheckedNativeContracts,
    call: &Document,
    expected: &EvidenceIdentity,
) -> IvyResult<Json> {
    NativeEvidence {
        store: world,
        catalogs,
    }
    .read(
        &call.pin.to_json(),
        call.value.required("evidence")?,
        expected,
    )
    .await
}

impl BlobStore for World<'_> {
    type Pin = Json;
    type Error = IvyError;
    fn save(&self, _: ChunkKey, _: &str, _: &JsString, _: &[u8]) -> IvyResult<Json> {
        panic!("Route migration must not create native evidence")
    }
    fn read(&self, key: ChunkKey, object: &Json, parent: &JsString) -> IvyResult<Vec<u8>> {
        assert_eq!(key, ChunkKey::NativeEvidence);
        assert_eq!(object, &pin(352, 1.0));
        assert_eq!(parent, &id(250));
        Ok(self
            .input()
            .required("migrationEvidenceBytes")?
            .as_str()?
            .into_bytes())
    }
}
impl ReadEvidenceStore for World<'_> {
    fn validate(&self, definition: &str, value: &Json) -> IvyResult<()> {
        contracts().validate_definition(definition, value)
    }
    async fn read_document(&self, key: &str, address: &Json, primary: bool) -> IvyResult<Document> {
        if key == "task-board/native-evidence" {
            if flag(self.input(), "migrationEvidenceMissing") {
                return Err(IvyError::new(
                    "revision_pruned",
                    "The exact evidence was pruned.",
                ));
            }
            assert_eq!(address, &pin(351, 1.0));
            let value = self.input().required("migrationEvidence")?;
            let mut document = doc(value);
            document.parent_id = value.get("parentId").and_then(Json::as_js_string).cloned();
            return Ok(document);
        }
        Ok(World::read(self, key, address, primary, true)
            .await?
            .document)
    }
    async fn write_document(&self, _: &str, _: &Json, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("Route migration must not write native evidence")
    }
}

fn contracts() -> &'static ivy_task_board::schema::RuntimeContracts {
    static VALUE: OnceLock<ivy_task_board::schema::RuntimeContracts> = OnceLock::new();
    VALUE.get_or_init(|| ivy_task_board::runtime_inputs::contracts().unwrap())
}

#[test]
fn sqlite_migration_fence_keeps_unknown_lifecycle_work_until_the_current_head_is_conclusive() {
    use ivy_store::rusqlite::Connection;
    use ivy_task_board::local_journal::{self, LocalJournal};
    let db = Connection::open_in_memory().unwrap();
    local_journal::initialize(&db).unwrap();
    let journal = LocalJournal {
        connection: &db,
        root_object_id: Some("root".into()),
    };
    let retained = data()
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g.required("rows").unwrap().as_array().unwrap())
        .find(|r| {
            s(r, "name")
                == &JsString::from("retained terminal call avoids preparation and inspection")
        })
        .unwrap();
    let original = retained
        .required("initial")
        .unwrap()
        .required("liveOperation")
        .unwrap()
        .required("value")
        .unwrap();
    let original_call = original
        .required("nativeCalls")
        .unwrap()
        .as_array()
        .unwrap()[0]
        .clone();
    let mut template = original_call.clone();
    template.insert("observation", Json::Null);
    let make = |task: JsString, public_id: &str, phase: &str, calls: Vec<Json>, unknown: bool| {
        let mut request = original.required("request").unwrap().clone();
        request.insert("taskId", Json::String(task));
        request.insert("operationId", public_id.into());
        let value = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("operationId", public_id.into()),
            ("callerPrincipalId", "caller".into()),
            ("requestHash", hash_json(&request).unwrap().into()),
            ("request", request),
            (
                "actor",
                Json::object([
                    ("principalId", "caller".into()),
                    ("serviceNodeId", "task-board".into()),
                    ("generation", Json::Number(1.0)),
                    ("source", "user".into()),
                ]),
            ),
            ("createdAt", original.required("createdAt").unwrap().clone()),
            ("updatedAt", original.required("createdAt").unwrap().clone()),
            ("phase", phase.into()),
            ("writes", Json::Array(vec![])),
            ("nativeCalls", Json::Array(calls)),
            (
                "outcome",
                if phase == "succeeded" {
                    Json::object([
                        ("operationId", public_id.into()),
                        ("task", Json::Null),
                        ("plan", Json::Null),
                        ("run", Json::Null),
                        ("history", Json::Null),
                        ("result", Json::Null),
                        ("review", Json::Null),
                        ("attachment", Json::Null),
                    ])
                } else {
                    Json::Null
                },
            ),
            (
                "error",
                if phase == "failed" {
                    Json::object([
                        ("code", "native_outcome".into()),
                        ("message", "Original lifecycle outcome.".into()),
                        (
                            "execution",
                            if unknown { "unknown" } else { "completed" }.into(),
                        ),
                    ])
                } else {
                    Json::Null
                },
            ),
        ]);
        contracts()
            .validate_definition("Operation", &value)
            .unwrap();
        value
    };
    let at = original
        .required("createdAt")
        .unwrap()
        .as_js_string()
        .unwrap();
    let task = id(100);
    let own = id(300);
    let other = id(301);
    let pending = id(302);
    let mine = make(task.clone(), "migration", "preparing", vec![], false);
    journal
        .create(
            "task-board/operation",
            &"mine".into(),
            Some(&"root".into()),
            &mine,
            at,
            &own,
        )
        .unwrap();
    let foreign = make(
        id(101),
        "foreign",
        "preparing",
        vec![template.clone()],
        false,
    );
    journal
        .create(
            "task-board/operation",
            &"foreign".into(),
            Some(&"root".into()),
            &foreign,
            at,
            &other,
        )
        .unwrap();
    assert!(!journal.route_migration_pending(&task, &own).unwrap());
    let unknown = make(
        task.clone(),
        "original",
        "failed",
        vec![original_call.clone()],
        true,
    );
    journal
        .create(
            "task-board/operation",
            &"pending".into(),
            Some(&"root".into()),
            &unknown,
            at,
            &pending,
        )
        .unwrap();
    assert!(
        journal.route_migration_pending(&task, &own).unwrap(),
        "An unknown domain outcome retains its migration fence even with a terminal native observation"
    );
    let unobserved = make(task.clone(), "original", "failed", vec![template], false);
    journal
        .update("task-board/operation", &pending, 1.0, &unobserved, at)
        .unwrap();
    assert!(
        journal.route_migration_pending(&task, &own).unwrap(),
        "A prepared lifecycle call without a conclusive original receipt cannot move"
    );
    let conclusive = make(
        task.clone(),
        "original",
        "failed",
        vec![original_call],
        false,
    );
    journal
        .update("task-board/operation", &pending, 2.0, &conclusive, at)
        .unwrap();
    assert!(
        !journal.route_migration_pending(&task, &own).unwrap(),
        "Historical unresolved revisions do not override the conclusive current head"
    );
    let accepted = make(task.clone(), "another", "accepted", vec![], false);
    journal
        .create(
            "task-board/operation",
            &"another".into(),
            Some(&"root".into()),
            &accepted,
            at,
            &id(303),
        )
        .unwrap();
    assert!(journal.route_migration_pending(&task, &own).unwrap());
    let finished = make(task.clone(), "another", "succeeded", vec![], false);
    journal
        .update("task-board/operation", &id(303), 1.0, &finished, at)
        .unwrap();
    assert!(!journal.route_migration_pending(&task, &own).unwrap());
}

fn scoped_plan(plan: &Json, contract: &ivy_contracts::native_contract::NativeContract) -> Json {
    let mut plan = plan.clone();
    if let Some(backend) = contract.backend() {
        plan.insert("backend", backend.json());
    }
    plan.insert(
        "definitions",
        Json::object(
            [
                ("threadStart", "thread/start"),
                ("threadResume", "thread/resume"),
                ("turnStart", "turn/start"),
                ("turnInterrupt", "turn/interrupt"),
            ]
            .map(|(key, method)| {
                (
                    key,
                    Json::from(contract.definition_hash(&method.into()).unwrap()),
                )
            }),
        ),
    );
    plan
}

fn scenario(
    catalogs: &CheckedNativeContracts,
    group: &Json,
    scoped: bool,
    fault: &str,
) -> (Json, Json, Vec<Json>) {
    let mut base = group.required("base").unwrap().clone();
    let version = s(base.required("plan").unwrap(), "nativeVersion").clone();
    let provider = if version == JsString::from("0.142.3") {
        NativeProvider::Claude
    } else {
        NativeProvider::Codex
    };
    let selected_id = if fault == "same-address" {
        "codex"
    } else {
        "joint"
    };
    let selected_node = if fault == "same-address" {
        s(
            base.required("allocation")
                .unwrap()
                .required("target")
                .unwrap(),
            "serviceNodeId",
        )
        .to_utf8()
        .unwrap()
    } else {
        "joint-agent".into()
    };
    let selected = BackendIdentity::new(selected_id.into(), provider, "primary".into()).unwrap();
    let old = scoped
        .then(|| BackendIdentity::new("previous".into(), provider, "primary".into()).unwrap());
    let catalog = catalogs
        .checked_native_contract(
            &version,
            Some(&Json::object([(
                "sourceHash",
                base.required("plan")
                    .unwrap()
                    .required("catalogSourceHash")
                    .unwrap()
                    .clone(),
            )])),
        )
        .unwrap();
    let old_catalog = catalog.with_backend(old.clone());
    let upgraded = fault.starts_with("upgrade");
    let current_catalog = if upgraded {
        catalogs
            .checked_native_contract(&"0.163.0".into(), None)
            .unwrap()
    } else {
        catalog.clone()
    };
    let new_catalog = current_catalog.with_backend(Some(selected.clone()));
    let old_plan = scoped_plan(base.required("plan").unwrap(), &old_catalog);
    let mut new_plan = scoped_plan(base.required("plan").unwrap(), &new_catalog);
    new_plan.insert(
        "nativeVersion",
        new_catalog.catalog().required("version").unwrap().clone(),
    );
    new_plan.insert(
        "catalogSourceHash",
        new_catalog
            .catalog()
            .required("sourceHash")
            .unwrap()
            .clone(),
    );
    if fault == "plan" {
        let mut template = new_plan.required("threadStart").unwrap().clone();
        template.insert("params", Json::object([("cwd", "/other".into())]));
        new_plan.insert("threadStart", template);
    }
    let draft = read_native_plan(
        &old_catalog,
        &old_plan,
        NativePlanKind::TaskBoard,
        Some(&"root".into()),
    )
    .unwrap();
    if upgraded {
        let mut current = scoped_plan(&draft, &new_catalog);
        for key in ["nativeVersion", "catalogSourceHash"] {
            current.insert(key, new_plan.required(key).unwrap().clone());
        }
        new_plan = save_native_plan(
            &new_catalog,
            &current,
            NativePlanKind::TaskBoard,
            Some(&"root".into()),
            &"new-route-plan".into(),
        )
        .unwrap();
    }
    base.insert("plan", new_plan);
    let mut target = base
        .required("allocation")
        .unwrap()
        .required("target")
        .unwrap()
        .clone();
    if let Some(old) = &old {
        target.insert("backend", old.json());
    }
    let mut current = selected
        .arguments(Json::object([
            ("serviceNodeId", selected_node.as_str().into()),
            ("hostId", target.required("hostId").unwrap().clone()),
        ]))
        .unwrap();
    if fault == "host" {
        current.insert("hostId", "foreign".into());
    }
    if fault == "account" {
        current.insert(
            "backend",
            BackendIdentity::new(selected_id.into(), provider, "foreign".into())
                .unwrap()
                .json(),
        );
    }
    let mut workspace = base
        .required("allocation")
        .unwrap()
        .required("workspace")
        .unwrap()
        .clone();
    for (key, value) in [
        ("hostId", target.required("hostId").unwrap().clone()),
        (
            "serviceNodeId",
            target.required("serviceNodeId").unwrap().clone(),
        ),
        ("taskRoot", "/workspace".into()),
        ("bootstrapPath", "/workspace".into()),
        ("sourcePath", Json::Null),
        ("project", Json::Null),
        ("useWorktree", false.into()),
        ("repository", Json::Null),
    ] {
        workspace.insert(key, value);
    }
    if ["project", "thread-project"].contains(&fault) {
        workspace.insert(
            "project",
            Json::object([
                (
                    "serviceNodeId",
                    target.required("serviceNodeId").unwrap().clone(),
                ),
                (
                    "namespace",
                    old.as_ref().map_or("codex", BackendIdentity::id).into(),
                ),
                ("kind", "project".into()),
                ("nativeId", "project".into()),
            ]),
        );
    }
    let mut moved_workspace = workspace.clone();
    moved_workspace.insert("serviceNodeId", selected_node.as_str().into());
    if ["project", "thread-project"].contains(&fault) {
        let mut project = workspace.required("project").unwrap().clone();
        project.insert("serviceNodeId", selected_node.as_str().into());
        project.insert("namespace", selected_id.into());
        moved_workspace.insert("project", project);
    }
    if fault == "workspace" {
        moved_workspace.insert("canonicalCwd", "/other".into());
    }
    let rows = group.required("rows").unwrap().as_array().unwrap();
    let original = rows
        .iter()
        .find(|r| s(r, "name") == &JsString::from("archive original durable thread"))
        .unwrap();
    let mut row = original.clone();
    let mut task = row
        .required("initial")
        .unwrap()
        .required("task")
        .unwrap()
        .clone();
    let mut value = task.required("value").unwrap().clone();
    value.remove("waiting");
    value.remove("blocker");
    let mut primary = value.required("primaryResourceRef").unwrap().clone();
    if let Some(old) = &old {
        primary.insert("namespace", old.id().into());
        value.insert("primaryBackend", old.json());
    }
    value.insert("primaryResourceRef", primary.clone());
    let fields = parse(r#"{"title":"Fixture","description":"","acceptanceCriteria":[],"category":null,"control":"agent","priority":2,"executionRequirement":{"kind":"host","hostId":"fixture"},"workspaceRequirement":{"kind":"directory_path","path":"/workspace"},"nativeOptions":{"model":null,"reasoningEffort":null,"serviceTier":"standard"},"userContact":"ticket","dueAt":null}"#, 4096).unwrap();
    value.insert("fields", fields);
    for (key, v) in [
        ("schemaVersion", Json::Number(1.0)),
        ("attemptCount", Json::Number(1.0)),
        ("agentCommentCount", Json::Number(0.0)),
        ("workRevision", Json::Number(1.0)),
        ("attachments", Json::Array(vec![])),
        ("comments", Json::Array(vec![])),
        ("commentDeliveries", Json::Array(vec![])),
        ("createdAt", "2026-01-02T03:04:05.006Z".into()),
        ("latestResult", Json::Null),
        ("acceptedReview", Json::Null),
        ("runtimeWait", Json::Null),
        ("wakeAt", Json::Null),
        ("dependencies", Json::Array(vec![])),
    ] {
        value.insert(key, v);
    }
    if fault == "claim" {
        value.insert("claim", Json::object([] as [(&str, Json); 0]));
    }
    task.insert("value", value.clone());
    if fault == "archived" {
        let mut metadata = task.required("metadata").unwrap().clone();
        metadata.insert("effectivelyArchived", true.into());
        metadata.insert("archivedAt", "2026-01-02T03:04:05.006Z".into());
        task.insert("metadata", metadata);
    }
    let original_request = Json::object([
        ("action", "start".into()),
        ("operationId", "start-ticket".into()),
        ("taskId", Json::String(id(100))),
        ("expectedRevision", Json::Number(1.0)),
        ("target", target.clone()),
        ("intent", pin(400, 1.0)),
        ("workspace", workspace.clone()),
        ("commentIds", Json::Array(vec![])),
    ]);
    let actor = Json::object([
        ("principalId", "caller".into()),
        ("serviceNodeId", "task-board".into()),
        ("generation", Json::Number(1.0)),
        ("source", "user".into()),
    ]);
    let mut run = Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("taskId", Json::String(id(100))),
        ("attempt", Json::Number(1.0)),
        ("operationId", "start-ticket".into()),
        ("callerPrincipalId", "caller".into()),
        ("requestHash", hash_json(&original_request).unwrap().into()),
        ("originalRequest", original_request),
        ("target", target.clone()),
        ("workspace", workspace),
        ("plan", draft),
        ("owner", actor),
        ("phase", "completed".into()),
        ("primaryResourceRef", primary),
        ("turnId", "turn".into()),
        ("nativeEpoch", "original-epoch".into()),
        (
            "calls",
            Json::object([
                ("thread", pin(250, 1.0)),
                ("turn", Json::Null),
                ("interrupt", Json::Null),
            ]),
        ),
        ("notificationCursor", Json::Number(0.0)),
        ("result", pin(350, 1.0)),
        ("cancellation", Json::Null),
        (
            "externalOutcome",
            Json::object([
                ("state", "succeeded".into()),
                ("observedAt", Json::Null),
                ("evidence", Json::Null),
                ("code", Json::Null),
            ]),
        ),
        ("createdAt", "2026-01-02T03:04:05.006Z".into()),
        ("updatedAt", "2026-01-02T03:04:05.006Z".into()),
        ("finishedAt", "2026-01-02T03:04:05.006Z".into()),
    ]);
    if fault == "active-run" {
        run.insert("phase", "running".into());
    }
    if fault == "unknown-run" {
        run.insert(
            "externalOutcome",
            Json::object([("state", "unknown".into())]),
        );
    }
    let params = Json::object([("cwd", "/workspace".into())]);
    let mut native_request = Json::object([
        ("nativeVersion", version.clone().into()),
        (
            "catalogSourceHash",
            old_plan.required("catalogSourceHash").unwrap().clone(),
        ),
        ("method", "thread/start".into()),
        (
            "params",
            save_native_parameters(
                &old_catalog,
                &"thread/start".into(),
                &params,
                &[],
                Some(&"root".into()),
            )
            .unwrap(),
        ),
    ]);
    if let Some(old) = &old {
        native_request.insert("backend", old.json());
    }
    let mut call = Json::object([
        ("operationId", "original-start".into()),
        ("request", native_request),
        (
            "expectedDefinitionHash",
            old_catalog
                .definition_hash(&"thread/start".into())
                .unwrap()
                .into(),
        ),
        ("ownerPhase", "succeeded".into()),
        ("state", "observed".into()),
        ("preparedAt", "2026-01-02T03:04:05.006Z".into()),
        ("observedAt", "2026-01-02T03:04:05.006Z".into()),
        ("epoch", "epoch".into()),
        ("code", Json::Null),
        (
            "evidence",
            Json::object([
                ("object", pin(351, 1.0)),
                ("label", "Original receipt".into()),
                ("mediaType", "application/json".into()),
                ("contentHash", format!("sha256:{}", "a".repeat(64)).into()),
            ]),
        ),
    ]);
    if let Some(old) = &old {
        call.insert(
            "owner",
            ivy_sdk::native_evidence::NativeOwnerIdentity {
                backend: Some(old.clone()),
                service_node_id: s(&target, "serviceNodeId").clone(),
                host_id: Some(s(&target, "hostId").clone()),
                native_version: version.clone(),
                native_executable_hash: s(old_catalog.catalog(), "nativeExecutableHash").clone(),
                catalog_hash: old_catalog.catalog_hash().into(),
            }
            .to_json(),
        );
    }
    if fault == "call-owner" {
        let mut owner = call.required("owner").unwrap().clone();
        owner.insert("serviceNodeId", "foreign".into());
        call.insert("owner", owner);
    }
    let request = Json::object([
        ("action", "migrateNativeRoute".into()),
        ("operationId", "migrate-ticket".into()),
        ("taskId", Json::String(id(100))),
        (
            "expectedRevision",
            Json::Number(if fault == "task-revision" { 3.0 } else { 2.0 }),
        ),
        ("run", pin(200, 1.0)),
        ("target", current),
        ("intent", pin(400, 1.0)),
        ("workspace", moved_workspace),
        ("reason", "Consolidate the owning service.".into()),
    ]);
    let mut initial = row.required("initial").unwrap().clone();
    if fault == "claim" {
        let mut task_value = task.required("value").unwrap().clone();
        task_value.insert("workflowState", "in_progress".into());
        task_value.insert(
            "claim",
            Json::object([
                ("operationId", "new-start".into()),
                ("callerPrincipalId", "caller".into()),
                ("attempt", Json::Number(2.0)),
                ("owner", run.required("owner").unwrap().clone()),
                ("request", run.required("originalRequest").unwrap().clone()),
                ("requestHash", run.required("requestHash").unwrap().clone()),
                ("target", target.clone()),
                ("workspace", run.required("workspace").unwrap().clone()),
                (
                    "primaryAtStart",
                    task_value.required("primaryResourceRef").unwrap().clone(),
                ),
                ("claimedAt", "2026-01-02T03:04:05.006Z".into()),
                ("phase", "claimed".into()),
                ("run", Json::Null),
            ]),
        );
        task.insert("value", task_value);
    }
    contracts()
        .validate_definition("Task", task.required("value").unwrap())
        .unwrap();
    contracts()
        .validate_definition("NativeCall", &call)
        .unwrap();
    initial.insert("task", task);
    for name in ["operation", "liveOperation"] {
        let mut op = initial.required(name).unwrap().clone();
        let at = Json::from("2026-01-02T03:04:05.006Z");
        let v = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("operationId", "migrate-ticket".into()),
            ("callerPrincipalId", "caller".into()),
            ("request", request.clone()),
            ("requestHash", hash_json(&request).unwrap().into()),
            ("actor", run.required("owner").unwrap().clone()),
            ("createdAt", at.clone()),
            ("updatedAt", at),
            ("phase", "preparing".into()),
            ("writes", Json::Array(vec![])),
            ("outcome", Json::Null),
            ("error", Json::Null),
            ("nativeCalls", Json::Array(vec![])),
        ]);
        op.insert("value", v);
        initial.insert(name, op);
    }
    row.insert("initial", initial);
    row.insert(
        "input",
        Json::object([
            (
                "migrationRun",
                Json::object([("pin", pin(200, 1.0)), ("value", run)]),
            ),
            (
                "migrationCall",
                Json::object([("pin", pin(250, 1.0)), ("value", call)]),
            ),
            ("migrationRunChanged", (fault == "run-revision").into()),
            ("migrationPending", (fault == "pending-lifecycle").into()),
        ]),
    );
    if fault == "older-unresolved" {
        let mut other = row
            .required("input")
            .unwrap()
            .required("migrationRun")
            .unwrap()
            .required("value")
            .unwrap()
            .clone();
        other.insert("phase", "outcome_unknown".into());
        let mut external = other.required("externalOutcome").unwrap().clone();
        external.insert("state", "unknown".into());
        other.insert("externalOutcome", external);
        let mut input = row.required("input").unwrap().clone();
        input.insert(
            "migrationOtherRun",
            Json::object([("pin", pin(500, 1.0)), ("value", other)]),
        );
        row.insert("input", input);
    }
    let results = parse(
        include_str!("task-board-route-migration-results.json"),
        65536,
    )
    .unwrap();
    let result_version = if upgraded {
        "0.163.0".to_owned()
    } else {
        version.to_utf8().unwrap()
    };
    let mut native_result = results.required(&result_version).unwrap().clone();
    if ["project", "thread-project"].contains(&fault) {
        let mut thread = native_result.required("thread").unwrap().clone();
        thread.insert("projectId", "project".into());
        native_result.insert("thread", thread);
    }
    if fault == "receipt-thread" {
        let mut t = native_result.required("thread").unwrap().clone();
        t.insert("id", "foreign".into());
        native_result.insert("thread", t);
    }
    let frames = original.required("rpcReplies").unwrap().as_array().unwrap();
    let mut replies = Vec::new();
    for (method, selector) in [
        ("serviceNodes.get", ""),
        ("tools.list", "status"),
        ("tools.call", "agent.status"),
        ("tools.list", "operation"),
        ("tools.call", "agent.operation"),
        ("tools.list", "read"),
        ("tools.call", "agent.read"),
    ] {
        let mut frame = frames
            .iter()
            .find(|r| {
                f(r, "method").is_str(method)
                    && (selector.is_empty()
                        || f(
                            f(r, "args"),
                            if method == "tools.list" {
                                "namePrefix"
                            } else {
                                "qualifiedName"
                            },
                        )
                        .is_str(selector))
            })
            .unwrap()
            .clone();
        let mut args = frame.required("args").unwrap().clone();
        args.insert("serviceNodeId", selected_node.as_str().into());
        if method == "tools.call" {
            let mut a = args.required("arguments").unwrap().clone();
            if selector == "agent.operation" {
                a.insert("operationId", "original-start".into());
            }
            if selector == "agent.read" {
                a.insert(
                    "nativeVersion",
                    new_catalog.catalog().required("version").unwrap().clone(),
                );
            }
            args.insert("arguments", selected.arguments(a).unwrap());
        }
        frame.insert("args", args);
        let mut v = frame.required("value").unwrap().clone();
        if method == "serviceNodes.get" {
            v.remove("nativeVersion");
        }
        if method == "tools.list" {
            v.insert(
                "provider",
                Json::object([(
                    "node",
                    Json::object([("serviceNodeId", selected_node.as_str().into())]),
                )]),
            );
        }
        if method == "tools.call" {
            if selector == "agent.operation" {
                v.insert("operationId", "original-start".into());
                v.insert("method", "thread/start".into());
                v.insert("params", params.clone());
                v.insert(
                    "requestHash",
                    hash_json(&Json::object([
                        ("method", "thread/start".into()),
                        ("params", params.clone()),
                    ]))
                    .unwrap()
                    .into(),
                );
                v.insert("reply", Json::object([("result", native_result.clone())]));
                if let Some(old) = &old {
                    v.insert("backend", old.json());
                }
                if fault == "receipt-owner" {
                    v.insert("serviceNodeId", "foreign".into());
                }
                if fault == "receipt-unknown" {
                    v.insert("phase", "outcome_unknown".into());
                    v.insert("reply", Json::Null);
                    v.insert("code", "outcome_unknown".into());
                }
            } else {
                v.insert("serviceNodeId", selected_node.as_str().into());
                v.insert("backend", selected.json());
                v.insert(
                    "nativeVersion",
                    new_catalog.catalog().required("version").unwrap().clone(),
                );
                v.insert(
                    "nativeExecutableHash",
                    new_catalog
                        .catalog()
                        .required("nativeExecutableHash")
                        .unwrap()
                        .clone(),
                );
                v.insert("catalogHash", new_catalog.catalog_hash().into());
                if selector == "agent.read" {
                    let mut thread = results
                        .required(&result_version)
                        .unwrap()
                        .required("thread")
                        .unwrap()
                        .clone();
                    if ["project", "thread-project"].contains(&fault) {
                        thread.insert(
                            "projectId",
                            if fault == "thread-project" {
                                "foreign"
                            } else {
                                "project"
                            }
                            .into(),
                        );
                    }
                    if fault == "active-thread" {
                        thread.insert(
                            "status",
                            Json::object([
                                ("type", "active".into()),
                                ("activeFlags", Json::Array(vec![])),
                            ]),
                        );
                    }
                    if fault == "thread-cwd" {
                        thread.insert("cwd", "/other".into());
                    }
                    v.insert(
                        "reply",
                        Json::object([("result", Json::object([("thread", thread)]))]),
                    );
                }
            }
        }
        frame.insert("value", v);
        replies.push(frame);
    }
    // Retain the actual original observation as the domain's immutable,
    // checksummed evidence. No manager receipt is available on the new route.
    let observation = replies
        .iter()
        .find(|frame| {
            f(frame, "method").is_str("tools.call")
                && f(f(frame, "args"), "qualifiedName").is_str("agent.operation")
        })
        .unwrap()
        .required("value")
        .unwrap()
        .clone();
    let bytes = canonical(&observation, MANAGEMENT_FRAME_BYTES).unwrap();
    let content_hash = ivy_contracts::hash::digest(bytes.as_bytes());
    let mut manifest = Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("format", "agent-operation/canonical-json".into()),
        ("contentHash", content_hash.clone().into()),
        ("byteLength", Json::Number(bytes.len() as f64)),
        (
            "chunks",
            Json::Array(vec![Json::object([
                ("object", pin(352, 1.0)),
                ("contentHash", content_hash.into()),
                ("byteLength", Json::Number(bytes.len() as f64)),
            ])]),
        ),
    ]);
    for key in [
        "operationId",
        "callerPrincipalId",
        "serviceNodeId",
        "nativeVersion",
        "nativeExecutableHash",
        "method",
        "requestHash",
        "phase",
        "epoch",
        "backend",
    ] {
        if let Some(value) = observation.get(key) {
            manifest.insert(key, value.clone());
        }
    }
    contracts()
        .validate_definition("NativeEvidence", &manifest)
        .unwrap();
    let mut input = row.required("input").unwrap().clone();
    let mut call = input.required("migrationCall").unwrap().clone();
    let mut value = call.required("value").unwrap().clone();
    value.insert(
        "evidence",
        Json::object([
            ("object", pin(351, 1.0)),
            ("label", "Original receipt".into()),
            ("mediaType", "application/json".into()),
            ("contentHash", hash_json(&manifest).unwrap().into()),
        ]),
    );
    call.insert("value", value);
    input.insert("migrationCall", call);
    input.insert(
        "migrationEvidence",
        Json::object([
            ("pin", pin(351, 1.0)),
            (
                "parentId",
                Json::String(if fault == "evidence-scope" {
                    id(999)
                } else {
                    id(250)
                }),
            ),
            ("value", manifest),
        ]),
    );
    input.insert(
        "migrationEvidenceBytes",
        if fault == "evidence-bytes" {
            format!("{bytes} ").into()
        } else {
            bytes.into()
        },
    );
    input.insert(
        "migrationEvidenceMissing",
        (fault == "evidence-missing").into(),
    );
    row.insert("input", input);
    replies.retain(|frame| {
        if (upgraded && !scoped) || fault == "collected" {
            return true;
        }
        !(f(frame, "method").is_str("tools.call")
            && f(f(frame, "args"), "qualifiedName").is_str("agent.operation"))
            && !(f(frame, "method").is_str("tools.list")
                && f(f(frame, "args"), "namePrefix").is_str("operation"))
    });
    (base, row, replies)
}

struct RetainedSteps<'a, 'b> {
    world: &'a World<'b>,
    rows: &'a RefCell<Vec<(String, Json, Json, Json)>>,
    index: usize,
    lose: usize,
}
impl LifecyclePublication for RetainedSteps<'_, '_> {
    async fn write_task_change(
        &mut self,
        value: &Json,
        destination: &Json,
        change: ivy_task_board::operations::TaskChange,
    ) -> IvyResult<Json> {
        assert!(matches!(
            change,
            ivy_task_board::operations::TaskChange::Header
        ));
        assert_eq!(value.get("commentStore"), Some(&compact_tree()));
        self.write("task-board/task", value, destination).await
    }

    fn pin(&self) -> IvyResult<Json> {
        Ok(pin(300, 1.0))
    }
    async fn write(&mut self, k: &str, v: &Json, d: &Json) -> IvyResult<Json> {
        let result = Json::object([
            ("objectId", d.required("objectId")?.clone()),
            ("revision", Json::Number(num(d, "expectedRevision") + 1.0)),
        ]);
        self.step(k, v, d, result)
    }
    async fn create(&mut self, k: &str, v: &Json, label: &JsString) -> IvyResult<Json> {
        self.step(k, v, &Json::String(label.clone()), pin(600, 1.0))
    }
}
impl RetainedSteps<'_, '_> {
    fn step(&mut self, k: &str, v: &Json, d: &Json, result: Json) -> IvyResult<Json> {
        contracts().validate_record(k, &contracts().version(k)?.into(), v)?;
        let index = self.index;
        self.index += 1;
        if let Some((old_key, old_value, old_destination, old_result)) =
            self.rows.borrow().get(index)
        {
            assert_eq!(old_key, k);
            assert_eq!(hash_json(old_value).unwrap(), hash_json(v).unwrap());
            assert_eq!(old_destination, d);
            return Ok(old_result.clone());
        }
        self.rows
            .borrow_mut()
            .push((k.into(), v.clone(), d.clone(), result.clone()));
        let mut op = self.world.live.borrow_mut();
        let mut value = op.required("value")?.clone();
        let mut writes = value.required("writes")?.as_array().unwrap().to_vec();
        writes.push(Json::object([
            ("mutationId", format!("prepared-{index}").into()),
            ("contractKey", k.into()),
            ("contractVersion", contracts().version(k)?.into()),
            (
                "payload",
                Json::object([
                    ("object", pin(700 + index as u64, 1.0)),
                    ("label", "Migration publication".into()),
                    ("mediaType", "application/json".into()),
                    ("contentHash", hash_json(v)?.into()),
                ]),
            ),
            (
                "destination",
                if d.as_js_string().is_some() {
                    Json::object([(
                        "create",
                        Json::object([("parentId", "root".into()), ("name", d.clone())]),
                    )])
                } else {
                    d.clone()
                },
            ),
            ("outcome", result.clone()),
        ]));
        value.insert("writes", Json::Array(writes));
        contracts().validate_definition("Operation", &value)?;
        op.insert("value", value);
        if self.lose == self.index {
            return Err(IvyError::new(
                "fixture_lost_reply",
                "The publication committed before its reply was lost.",
            ));
        }
        Ok(result)
    }
}

async fn archive_migrated_history(
    catalogs: &CheckedNativeContracts,
    group: &Json,
    task_value: &Json,
    run: &Json,
    original_run: &Json,
) {
    let original = group
        .required("rows")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|r| s(r, "name") == &JsString::from("archive already archived"))
        .unwrap();
    let mut row = original.clone();
    let mut initial = row.required("initial").unwrap().clone();
    let mut task = initial.required("task").unwrap().clone();
    task.insert("value", task_value.clone());
    initial.insert("task", task);
    row.insert("initial", initial);
    row.insert(
        "input",
        Json::object([
            (
                "migrationRun",
                Json::object([("pin", pin(200, 1.0)), ("value", run.clone())]),
            ),
            (
                "migrationOtherRun",
                Json::object([("pin", pin(500, 1.0)), ("value", original_run.clone())]),
            ),
        ]),
    );
    let backend =
        BackendIdentity::from_json(task_value.required("primaryBackend").unwrap()).unwrap();
    let target = task_value
        .required("nativeRoutes")
        .unwrap()
        .as_array()
        .unwrap()[0]
        .required("target")
        .unwrap();
    let mut replies = Vec::new();
    for frame in original.required("rpcReplies").unwrap().as_array().unwrap() {
        let mut frame = frame.clone();
        let method = s(&frame, "method").to_utf8().unwrap();
        if method == "serviceNodes.get" || method.starts_with("tools.") {
            let mut args = frame.required("args").unwrap().clone();
            args.insert(
                "serviceNodeId",
                target.required("serviceNodeId").unwrap().clone(),
            );
            if method == "tools.call" {
                args.insert(
                    "arguments",
                    backend
                        .arguments(args.required("arguments").unwrap().clone())
                        .unwrap(),
                );
            }
            frame.insert("args", args);
            let mut value = frame.required("value").unwrap().clone();
            if method == "serviceNodes.get" {
                value.remove("nativeVersion");
            }
            if method == "tools.list" {
                value.insert(
                    "provider",
                    Json::object([(
                        "node",
                        Json::object([(
                            "serviceNodeId",
                            target.required("serviceNodeId").unwrap().clone(),
                        )]),
                    )]),
                );
            }
            if method == "tools.call" {
                value.insert(
                    "serviceNodeId",
                    target.required("serviceNodeId").unwrap().clone(),
                );
                value.insert("backend", backend.json());
            }
            frame.insert("value", value);
        }
        replies.push(frame);
    }
    let trace: Trace = Arc::default();
    let client = Arc::new(Client {
        case_name: "archive migrated historical route".into(),
        replies: Mutex::new(replies.into()),
        trace: trace.clone(),
    });
    let world = World {
        base: group.required("base").unwrap(),
        row: &row,
        live: RefCell::new(
            row.required("initial")
                .unwrap()
                .required("liveOperation")
                .unwrap()
                .clone(),
        ),
        trace,
        tools: Arc::new(ToolClients::new(client.clone())),
        client,
        consumed: Cell::new(false),
        cleanups: RefCell::default(),
    };
    let subject = NativeLifecycle {
        store: &world,
        engine: &world,
        operations: &world,
        catalogs,
    };
    let mut steps = Steps {
        world: &world,
        count: 0,
    };
    subject
        .publish(
            &doc(world.initial("operation")),
            &mut steps,
            Json::object([
                ("task", Json::Null),
                ("history", Json::Null),
                ("archive", Json::Null),
            ]),
        )
        .await
        .unwrap();
    assert_eq!(
        world.cleanups.borrow().len(),
        1,
        "Current and historical addresses must deduplicate at the selected account"
    );
    assert!(BackendIdentity::matches(
        &world.cleanups.borrow()[0],
        Some(&backend)
    ));
    assert_eq!(
        world.cleanups.borrow()[0]
            .required("serviceNodeId")
            .unwrap(),
        target.required("serviceNodeId").unwrap()
    );
    assert!(world.client.replies.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn migration_preserves_originals_and_resumes_each_committed_publication_without_native_replay()
 {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    for group in data().as_array().unwrap().iter().filter(|g| {
        ["0.142.3", "0.163.0"]
            .iter()
            .any(|v| f(f(g, "base").required("plan").unwrap(), "nativeVersion").is_str(v))
    }) {
        for scoped in [false, true] {
            for (lose, compact) in [
                (0, false),
                (0, true),
                (1, false),
                (1, true),
                (2, false),
                (2, true),
                (3, false),
                (3, true),
            ] {
                for archived in [false, true] {
                    let mode = if archived {
                        "archived"
                    } else if scoped && lose == 0 {
                        "project"
                    } else {
                        ""
                    };
                    let (base, mut row, replies) = scenario(&catalogs, group, scoped, mode);
                    row.get_mut("input")
                        .unwrap()
                        .insert("compactHeader", compact.into());
                    let trace: Trace = Arc::default();
                    let client = Arc::new(Client {
                        case_name: "native route migration".into(),
                        replies: Mutex::new(replies.into()),
                        trace: trace.clone(),
                    });
                    let world = World {
                        base: &base,
                        row: &row,
                        live: RefCell::new(
                            row.required("initial")
                                .unwrap()
                                .required("liveOperation")
                                .unwrap()
                                .clone(),
                        ),
                        trace,
                        tools: Arc::new(ToolClients::new(client.clone())),
                        client,
                        consumed: Cell::new(false),
                        cleanups: RefCell::default(),
                    };
                    let subject = NativeLifecycle {
                        store: &world,
                        engine: &world,
                        operations: &world,
                        catalogs: &catalogs,
                    };
                    let source = world
                        .input()
                        .required("migrationRun")
                        .unwrap()
                        .required("value")
                        .unwrap()
                        .clone();
                    let call = world.input().required("migrationCall").unwrap().clone();
                    let rows = RefCell::new(vec![]);
                    let mut steps = RetainedSteps {
                        world: &world,
                        rows: &rows,
                        index: 0,
                        lose,
                    };
                    let empty = Json::object([
                        ("task", Json::Null),
                        ("history", Json::Null),
                        ("run", Json::Null),
                    ]);
                    let result = subject
                        .publish(&doc(world.initial("operation")), &mut steps, empty.clone())
                        .await;
                    if lose > 0 {
                        assert_eq!(result.unwrap_err().code, "fixture_lost_reply");
                        assert!(world.client.replies.lock().unwrap().is_empty());
                        let retained = doc(&world.live.borrow());
                        let mut resumed = RetainedSteps {
                            world: &world,
                            rows: &rows,
                            index: 0,
                            lose: 0,
                        };
                        subject
                            .publish(&retained, &mut resumed, empty)
                            .await
                            .unwrap();
                    } else {
                        result.unwrap();
                    }
                    let saved = rows.borrow();
                    assert_eq!(saved.len(), 3);
                    let task = &saved[2].1;
                    if compact {
                        assert_eq!(saved[0].1.get("commentStore"), Some(&compact_tree()));
                        assert_eq!(task.get("commentStore"), Some(&compact_tree()));
                    }
                    for write in [&saved[0], &saved[2]] {
                        assert_eq!(write.2.get("expectedArchived"), Some(&archived.into()));
                    }
                    assert!(saved.iter().all(|(key, _, _, _)| key != "task-board/run"));
                    assert_eq!(task.get("lastRun"), world.task().value.get("lastRun"));
                    assert_eq!(
                        world
                            .input()
                            .required("migrationRun")
                            .unwrap()
                            .required("value")
                            .unwrap(),
                        &source
                    );
                    assert_eq!(world.input().required("migrationCall").unwrap(), &call);
                    assert_eq!(
                        task.required("primaryResourceRef")
                            .unwrap()
                            .required("nativeId")
                            .unwrap(),
                        &"thread".into()
                    );
                    assert_eq!(
                        task.required("primaryBackend")
                            .unwrap()
                            .required("accountId")
                            .unwrap(),
                        &"primary".into()
                    );
                    assert_eq!(
                        task.get("workflowState"),
                        world.task().value.get("workflowState")
                    );
                    assert_eq!(saved[1].1.required("priorRun").unwrap(), &pin(200, 1.0));
                    assert!(world.client.replies.lock().unwrap().is_empty());
                    assert!(world.cleanups.borrow().is_empty());
                    assert!(
                        world
                            .live
                            .borrow()
                            .required("value")
                            .unwrap()
                            .required("nativeCalls")
                            .unwrap()
                            .as_array()
                            .unwrap()
                            .is_empty()
                    );
                    if lose == 0 && !scoped {
                        archive_migrated_history(&catalogs, group, task, &source, &source).await;
                    }
                }
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn migration_refuses_changed_owners_paths_work_or_original_receipts_before_ticket_writes() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    for fault in [
        "claim",
        "active-run",
        "unknown-run",
        "account",
        "host",
        "workspace",
        "plan",
        "task-revision",
        "run-revision",
        "receipt-thread",
        "receipt-owner",
        "call-owner",
        "receipt-unknown",
        "evidence-scope",
        "evidence-bytes",
        "evidence-missing",
        "active-thread",
        "thread-cwd",
        "thread-project",
        "older-unresolved",
        "pending-lifecycle",
    ] {
        let (base, row, replies) = scenario(&catalogs, &data().as_array().unwrap()[0], true, fault);
        let trace: Trace = Arc::default();
        let client = Arc::new(Client {
            case_name: fault.into(),
            replies: Mutex::new(replies.into()),
            trace: trace.clone(),
        });
        let world = World {
            base: &base,
            row: &row,
            live: RefCell::new(
                row.required("initial")
                    .unwrap()
                    .required("liveOperation")
                    .unwrap()
                    .clone(),
            ),
            trace,
            tools: Arc::new(ToolClients::new(client.clone())),
            client,
            consumed: Cell::new(false),
            cleanups: RefCell::default(),
        };
        let rows = RefCell::new(vec![]);
        let mut steps = RetainedSteps {
            world: &world,
            rows: &rows,
            index: 0,
            lose: 0,
        };
        let subject = NativeLifecycle {
            store: &world,
            engine: &world,
            operations: &world,
            catalogs: &catalogs,
        };
        let error = subject
            .publish(
                &doc(world.initial("operation")),
                &mut steps,
                Json::Object(vec![]),
            )
            .await
            .unwrap_err();
        let expected = match fault {
            "claim" | "older-unresolved" | "pending-lifecycle" => "task_board_execution_unresolved",
            "active-run" | "unknown-run" => "task_board_route_migration_unresolved",
            "account" | "host" => "task_board_target_mismatch",
            "workspace" => "task_board_workspace_mismatch",
            "plan" => "task_board_native_intent_mismatch",
            "task-revision" | "run-revision" => "revision_conflict",
            "receipt-owner" | "call-owner" | "evidence-scope" | "evidence-bytes" => {
                "task_board_evidence_mismatch"
            }
            "evidence-missing" => "revision_pruned",
            "receipt-unknown" => "task_board_native_outcome_unresolved",
            _ => "task_board_primary_mismatch",
        };
        assert_eq!(error.code, expected, "{fault}: {error:?}");
        assert!(rows.borrow().is_empty(), "{fault}");
        assert!(world.cleanups.borrow().is_empty());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn collected_calls_use_authenticated_storage_provenance_and_replay_publications() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    for group in data().as_array().unwrap().iter().filter(|g| {
        ["0.142.3", "0.163.0"]
            .iter()
            .any(|v| f(f(g, "base").required("plan").unwrap(), "nativeVersion").is_str(v))
    }) {
        for scoped in [false, true] {
            for (fault, expected) in [
                ("", None),
                ("pruned", None),
                ("resume", None),
                ("lose-1", None),
                ("lose-2", None),
                ("lose-3", None),
                ("origin", None),
                ("origin-null-call", None),
                ("origin-pruned", None),
                ("origin-upgrade", None),
                ("origin-archived", None),
                ("origin-lose-1", None),
                ("origin-lose-2", None),
                ("origin-lose-3", None),
                ("origin-owner", Some("task_board_evidence_mismatch")),
                ("origin-provider", Some("task_board_evidence_mismatch")),
                ("origin-account", Some("task_board_evidence_mismatch")),
                ("origin-route", Some("task_board_evidence_mismatch")),
                ("origin-missing", Some("native_state_migration_required")),
                ("origin-uncertain-absence", Some("not_found")),
                ("origin-active-thread", Some("task_board_primary_mismatch")),
                ("origin-thread-cwd", Some("task_board_primary_mismatch")),
                ("origin-thread-project", Some("task_board_primary_mismatch")),
                ("origin-claim", Some("task_board_execution_unresolved")),
                (
                    "origin-unknown-run",
                    Some("task_board_route_migration_unresolved"),
                ),
                ("uncertain-absence", Some("not_found")),
                ("read-failed", Some("fixture_read_failed")),
                ("surviving-call", Some("task_board_evidence_mismatch")),
                ("caller", Some("native_evidence_mismatch")),
                ("owner", Some("native_evidence_mismatch")),
                ("backend", Some("native_evidence_mismatch")),
                ("executable", Some("native_evidence_mismatch")),
                ("hash", Some("native_evidence_mismatch")),
                ("epoch", Some("task_board_evidence_mismatch")),
                ("method", Some("task_board_evidence_mismatch")),
                ("unknown", Some("task_board_native_outcome_unresolved")),
                ("thread", Some("task_board_primary_mismatch")),
                ("resume-thread", Some("task_board_primary_mismatch")),
                ("operation-id", Some("native_evidence_mismatch")),
                ("storage-missing", Some("not_found")),
            ] {
                let storage_origin = fault.starts_with("origin");
                let mode = if fault == "origin-upgrade" {
                    "upgrade"
                } else if let Some(mode) = fault.strip_prefix("origin-").filter(|v| {
                    [
                        "archived",
                        "active-thread",
                        "thread-cwd",
                        "thread-project",
                        "claim",
                        "unknown-run",
                    ]
                    .contains(v)
                }) {
                    mode
                } else {
                    "collected"
                };
                let (base, mut row, mut replies) = scenario(&catalogs, group, scoped, mode);
                let mut original_run = row
                    .required("input")
                    .unwrap()
                    .required("migrationRun")
                    .unwrap()
                    .clone();
                let mut input = row.required("input").unwrap().clone();
                if fault == "origin-null-call" {
                    let mut run = original_run.required("value").unwrap().clone();
                    let mut calls = run.required("calls").unwrap().clone();
                    calls.insert("thread", Json::Null);
                    run.insert("calls", calls);
                    original_run.insert("value", run);
                    input.insert("migrationRun", original_run.clone());
                }
                input.insert(
                    "migrationCallUncertain",
                    fault.ends_with("uncertain-absence").into(),
                );
                if fault != "surviving-call" {
                    input.insert(
                        "migrationCallError",
                        if fault.ends_with("pruned") {
                            "revision_pruned"
                        } else if fault == "read-failed" {
                            "fixture_read_failed"
                        } else {
                            "not_found"
                        }
                        .into(),
                    );
                    input.remove("migrationCall");
                }
                input.remove("migrationEvidence");
                input.remove("migrationEvidenceBytes");
                row.insert("input", input);
                let mut initial = row.required("initial").unwrap().clone();
                for name in ["operation", "liveOperation"] {
                    let mut op = initial.required(name).unwrap().clone();
                    let mut value = op.required("value").unwrap().clone();
                    let mut request = value.required("request").unwrap().clone();
                    if !storage_origin {
                        request.insert(
                            "originalThreadOperationId",
                            if fault == "surviving-call" {
                                "different-start"
                            } else {
                                "original-start"
                            }
                            .into(),
                        );
                    }
                    contracts()
                        .validate_definition("MigrateNativeRouteRequest", &request)
                        .unwrap();
                    value.insert("requestHash", hash_json(&request).unwrap().into());
                    value.insert("request", request);
                    op.insert("value", value);
                    initial.insert(name, op);
                }
                row.insert("initial", initial);
                if storage_origin {
                    let mut listing = replies
                        .iter()
                        .find(|frame| {
                            f(frame, "method").is_str("tools.list")
                                && f(f(frame, "args"), "namePrefix").is_str("read")
                        })
                        .unwrap()
                        .clone();
                    let mut args = listing.required("args").unwrap().clone();
                    args.insert("namePrefix", "storageIdentity".into());
                    listing.insert("args", args);
                    let mut value = listing.required("value").unwrap().clone();
                    let mut item = value.required("items").unwrap().as_array().unwrap()[0].clone();
                    item.insert("qualifiedName", "agent.storageIdentity".into());
                    let mut definition = item.required("definition").unwrap().clone();
                    definition.insert("name", "storageIdentity".into());
                    item.insert("definition", definition);
                    value.insert("items", Json::Array(vec![item]));
                    listing.insert("value", value);
                    let mut query = replies
                        .iter()
                        .find(|frame| f(f(frame, "args"), "qualifiedName").is_str("agent.read"))
                        .unwrap()
                        .clone();
                    let mut args = query.required("args").unwrap().clone();
                    args.insert("qualifiedName", "agent.storageIdentity".into());
                    let backend = args
                        .required("arguments")
                        .unwrap()
                        .required("backend")
                        .unwrap()
                        .clone();
                    args.insert("arguments", Json::object([("backend", backend.clone())]));
                    let origin = Json::object([
                        (
                            "serviceNodeId",
                            args.required("serviceNodeId").unwrap().clone(),
                        ),
                        (
                            "originalServiceNodeId",
                            original_run
                                .required("value")
                                .unwrap()
                                .required("target")
                                .unwrap()
                                .required("serviceNodeId")
                                .unwrap()
                                .clone(),
                        ),
                        ("provider", backend.required("provider").unwrap().clone()),
                        ("backend", backend),
                    ]);
                    query.insert("args", args);
                    query.insert("value", origin);
                    let mut origin = query.required("value").unwrap().clone();
                    match fault {
                        "origin-owner" => {
                            origin.insert("originalServiceNodeId", "foreign".into());
                        }
                        "origin-route" => {
                            origin.insert("serviceNodeId", "foreign".into());
                        }
                        "origin-provider" => {
                            origin.insert(
                                "provider",
                                if f(&origin, "provider").is_str("codex") {
                                    "claude"
                                } else {
                                    "codex"
                                }
                                .into(),
                            );
                        }
                        "origin-account" => {
                            let mut backend = origin.required("backend").unwrap().clone();
                            backend.insert("accountId", "foreign".into());
                            origin.insert("backend", backend);
                        }
                        _ => {}
                    }
                    query.insert("value", origin);
                    if fault == "origin-missing" {
                        query.remove("value");
                        query.insert(
                            "error",
                            Json::object([
                                ("code", "native_state_migration_required".into()),
                                ("message", "The original provider was never seeded.".into()),
                                ("outcome", "not_executed".into()),
                                ("details", Json::Null),
                            ]),
                        );
                    }
                    replies.retain(|frame| {
                        !f(f(frame, "args"), "qualifiedName").is_str("agent.operation")
                            && !(f(frame, "method").is_str("tools.list")
                                && f(f(frame, "args"), "namePrefix").is_str("operation"))
                    });
                    replies.splice(3..3, [listing, query]);
                }
                for frame in &mut replies {
                    if !f(f(frame, "args"), "qualifiedName").is_str("agent.operation") {
                        continue;
                    }
                    if fault == "storage-missing" {
                        frame.remove("value");
                        frame.insert(
                            "error",
                            Json::object([
                                ("code", "not_found".into()),
                                ("message", "No original storage receipt remains.".into()),
                                ("outcome", "not_executed".into()),
                                ("details", Json::Null),
                            ]),
                        );
                        continue;
                    }
                    let mut value = frame.required("value").unwrap().clone();
                    value.insert(
                        "epoch",
                        original_run
                            .required("value")
                            .unwrap()
                            .required("nativeEpoch")
                            .unwrap()
                            .clone(),
                    );
                    if ["resume", "resume-thread"].contains(&fault) {
                        value.insert("method", "thread/resume".into());
                        let mut params = value.required("params").unwrap().clone();
                        params.insert(
                            "threadId",
                            if fault == "resume-thread" {
                                "foreign"
                            } else {
                                "thread"
                            }
                            .into(),
                        );
                        params.insert("excludeTurns", true.into());
                        value.insert(
                            "requestHash",
                            hash_json(&Json::object([
                                ("method", "thread/resume".into()),
                                ("params", params.clone()),
                            ]))
                            .unwrap()
                            .into(),
                        );
                        value.insert("params", params);
                    }
                    match fault {
                        "operation-id" => {
                            value.insert("operationId", "foreign".into());
                        }
                        "caller" => {
                            value.insert("callerPrincipalId", "foreign".into());
                        }
                        "owner" => {
                            value.insert("serviceNodeId", "foreign".into());
                        }
                        "backend" => {
                            value.insert(
                                "backend",
                                BackendIdentity::new(
                                    "foreign".into(),
                                    NativeProvider::Codex,
                                    "other".into(),
                                )
                                .unwrap()
                                .json(),
                            );
                        }
                        "executable" => {
                            value.insert(
                                "nativeExecutableHash",
                                format!("sha256:{}", "a".repeat(64)).into(),
                            );
                        }
                        "hash" => {
                            value
                                .insert("requestHash", format!("sha256:{}", "a".repeat(64)).into());
                        }
                        "epoch" => {
                            value.insert("epoch", "foreign-epoch".into());
                        }
                        "method" => {
                            value.insert("method", "turn/start".into());
                        }
                        "unknown" => {
                            value.insert("phase", "outcome_unknown".into());
                            value.insert("reply", Json::Null);
                            value.insert("code", "outcome_unknown".into());
                        }
                        "thread" => {
                            let mut reply = value.required("reply").unwrap().clone();
                            let mut result = reply.required("result").unwrap().clone();
                            let mut thread = result.required("thread").unwrap().clone();
                            thread.insert("id", "foreign".into());
                            result.insert("thread", thread);
                            reply.insert("result", result);
                            value.insert("reply", reply);
                        }
                        _ => {}
                    }
                    frame.insert("value", value);
                }
                let trace: Trace = Arc::default();
                let client = Arc::new(Client {
                    case_name: fault.into(),
                    replies: Mutex::new(replies.into()),
                    trace: trace.clone(),
                });
                let world = World {
                    base: &base,
                    row: &row,
                    live: RefCell::new(
                        row.required("initial")
                            .unwrap()
                            .required("liveOperation")
                            .unwrap()
                            .clone(),
                    ),
                    trace,
                    tools: Arc::new(ToolClients::new(client.clone())),
                    client,
                    consumed: Cell::new(false),
                    cleanups: RefCell::default(),
                };
                let subject = NativeLifecycle {
                    store: &world,
                    engine: &world,
                    operations: &world,
                    catalogs: &catalogs,
                };
                let rows = RefCell::new(vec![]);
                let lose = fault
                    .strip_prefix("origin-")
                    .unwrap_or(fault)
                    .strip_prefix("lose-")
                    .map(|v| v.parse().unwrap())
                    .unwrap_or(0);
                let mut steps = RetainedSteps {
                    world: &world,
                    rows: &rows,
                    index: 0,
                    lose,
                };
                let result = subject
                    .publish(
                        &doc(world.initial("operation")),
                        &mut steps,
                        Json::object([] as [(&str, Json); 0]),
                    )
                    .await;
                if let Some(code) = expected {
                    assert_eq!(result.unwrap_err().code, code, "{scoped}/{fault}");
                    assert!(rows.borrow().is_empty(), "{fault}");
                } else {
                    if lose > 0 {
                        assert_eq!(result.unwrap_err().code, "fixture_lost_reply");
                        let retained = doc(&world.live.borrow());
                        let mut resumed = RetainedSteps {
                            world: &world,
                            rows: &rows,
                            index: 0,
                            lose: 0,
                        };
                        subject
                            .publish(
                                &retained,
                                &mut resumed,
                                Json::object([] as [(&str, Json); 0]),
                            )
                            .await
                            .unwrap();
                    } else {
                        result.unwrap();
                    }
                    assert_eq!(rows.borrow().len(), 3);
                    assert!(world.client.replies.lock().unwrap().is_empty());
                }
                assert_eq!(
                    world.input().required("migrationRun").unwrap(),
                    &original_run
                );
                assert!(world.cleanups.borrow().is_empty());
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn independent_historical_owners_and_repeated_moves_preserve_current_work_and_original_records()
 {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    for group in data().as_array().unwrap().iter().filter(|g| {
        ["0.142.3", "0.163.0"]
            .iter()
            .any(|v| f(f(g, "base").required("plan").unwrap(), "nativeVersion").is_str(v))
    }) {
        for historical in [false, true] {
            for lose in 0..=3 {
                let (base, mut row, replies) = scenario(&catalogs, group, true, "");
                let original_run = row
                    .required("input")
                    .unwrap()
                    .required("migrationRun")
                    .unwrap()
                    .clone();
                let source = original_run
                    .required("value")
                    .unwrap()
                    .required("target")
                    .unwrap();
                let old_backend =
                    BackendIdentity::from_json(source.required("backend").unwrap()).unwrap();
                let previous = BackendIdentity::new(
                    "intermediate".into(),
                    if historical {
                        if old_backend.provider() == NativeProvider::Claude {
                            NativeProvider::Codex
                        } else {
                            NativeProvider::Claude
                        }
                    } else {
                        old_backend.provider()
                    },
                    if historical { "other" } else { "primary" }.into(),
                )
                .unwrap();
                let owner = Json::object([
                    ("hostId", source.required("hostId").unwrap().clone()),
                    ("serviceNodeId", "intermediate-agent".into()),
                    ("backend", previous.json()),
                ]);
                let mut initial = row.required("initial").unwrap().clone();
                let mut ticket = initial.required("task").unwrap().clone();
                let mut value = ticket.required("value").unwrap().clone();
                let mut primary = value.required("primaryResourceRef").unwrap().clone();
                primary.insert("serviceNodeId", "intermediate-agent".into());
                primary.insert("namespace", previous.id().into());
                if historical {
                    primary.insert("nativeId", "other-thread".into());
                }
                value.insert("primaryResourceRef", primary.clone());
                value.insert("primaryBackend", previous.json());
                let mut fields = value.required("fields").unwrap().clone();
                let mut options = fields.required("nativeOptions").unwrap().clone();
                options.insert("backend", previous.json());
                fields.insert("nativeOptions", options);
                value.insert("fields", fields);
                let mut earlier = source.clone();
                if historical {
                    earlier.insert("serviceNodeId", "other-original-agent".into());
                    earlier.insert("backend", previous.json());
                    value.insert("lastRun", pin(500, 1.0));
                    let mut input = row.required("input").unwrap().clone();
                    let mut other = original_run.clone();
                    other.insert("pin", pin(500, 1.0));
                    let mut other_value = other.required("value").unwrap().clone();
                    let mut other_primary = primary.clone();
                    other_primary.insert("serviceNodeId", "other-original-agent".into());
                    other_value.insert("target", earlier.clone());
                    other_value.insert("primaryResourceRef", other_primary);
                    other.insert("value", other_value);
                    input.insert("migrationOtherRun", other);
                    row.insert("input", input);
                }
                value.insert(
                    "nativeRoutes",
                    Json::Array(vec![Json::object([
                        ("source", earlier),
                        ("target", owner.clone()),
                    ])]),
                );
                ticket.insert("value", value.clone());
                initial.insert("task", ticket);
                row.insert("initial", initial);
                let original = row.required("input").unwrap().clone();
                let trace: Trace = Arc::default();
                let client = Arc::new(Client {
                    case_name: "multiple original native owners".into(),
                    replies: Mutex::new(replies.into()),
                    trace: trace.clone(),
                });
                let world = World {
                    base: &base,
                    row: &row,
                    live: RefCell::new(
                        row.required("initial")
                            .unwrap()
                            .required("liveOperation")
                            .unwrap()
                            .clone(),
                    ),
                    trace,
                    tools: Arc::new(ToolClients::new(client.clone())),
                    client,
                    consumed: Cell::new(false),
                    cleanups: RefCell::default(),
                };
                let subject = NativeLifecycle {
                    store: &world,
                    engine: &world,
                    operations: &world,
                    catalogs: &catalogs,
                };
                let rows = RefCell::new(vec![]);
                let mut steps = RetainedSteps {
                    world: &world,
                    rows: &rows,
                    index: 0,
                    lose,
                };
                let result = subject
                    .publish(
                        &doc(world.initial("operation")),
                        &mut steps,
                        Json::Object(vec![]),
                    )
                    .await;
                if lose > 0 {
                    assert_eq!(result.unwrap_err().code, "fixture_lost_reply");
                    let retained = doc(&world.live.borrow());
                    let mut resumed = RetainedSteps {
                        world: &world,
                        rows: &rows,
                        index: 0,
                        lose: 0,
                    };
                    subject
                        .publish(&retained, &mut resumed, Json::Object(vec![]))
                        .await
                        .unwrap();
                } else {
                    result.unwrap();
                }
                let saved = rows.borrow();
                let task = &saved[2].1;
                assert_eq!(task.get("lastRun"), value.get("lastRun"));
                assert_eq!(world.input(), &original);
                assert!(saved.iter().all(|(key, _, _, _)| key != "task-board/run"));
                let routes = task.required("nativeRoutes").unwrap().as_array().unwrap();
                assert_eq!(routes.len(), 2);
                let target = world
                    .initial("operation")
                    .required("value")
                    .unwrap()
                    .required("request")
                    .unwrap()
                    .required("target")
                    .unwrap();
                assert_eq!(routes[1].get("target"), Some(target));
                if historical {
                    for key in ["primaryResourceRef", "primaryBackend", "fields"] {
                        assert_eq!(task.get(key), value.get(key));
                    }
                    assert_eq!(routes[0].get("target"), Some(&owner));
                    assert!(routes[0].get("catalog").is_none());
                } else {
                    assert_eq!(routes[0].get("target"), Some(target));
                    assert_eq!(routes[0].get("catalog"), routes[1].get("catalog"));
                    assert_eq!(routes[1].get("source"), Some(&owner));
                    assert_eq!(
                        task.required("primaryResourceRef")
                            .unwrap()
                            .get("serviceNodeId"),
                        target.get("serviceNodeId")
                    );
                }
                assert!(world.client.replies.lock().unwrap().is_empty());
                assert!(world.cleanups.borrow().is_empty());
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn migration_reads_retained_evidence_and_requires_original_storage_provenance_for_unscoped_engine_changes()
 {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let group = data()
        .as_array()
        .unwrap()
        .iter()
        .find(|group| {
            f(f(group, "base").required("plan").unwrap(), "nativeVersion").is_str("0.159.2")
        })
        .unwrap();
    for scoped in [false, true] {
        let (base, row, replies) = scenario(&catalogs, group, scoped, "upgrade");
        let trace: Trace = Arc::default();
        let client = Arc::new(Client {
            case_name: "native upgrade route migration".into(),
            replies: Mutex::new(replies.into()),
            trace: trace.clone(),
        });
        let world = World {
            base: &base,
            row: &row,
            live: RefCell::new(
                row.required("initial")
                    .unwrap()
                    .required("liveOperation")
                    .unwrap()
                    .clone(),
            ),
            trace,
            tools: Arc::new(ToolClients::new(client.clone())),
            client,
            consumed: Cell::new(false),
            cleanups: RefCell::default(),
        };
        let subject = NativeLifecycle {
            store: &world,
            engine: &world,
            operations: &world,
            catalogs: &catalogs,
        };
        let rows = RefCell::new(vec![]);
        let mut steps = RetainedSteps {
            world: &world,
            rows: &rows,
            index: 0,
            lose: 0,
        };
        let original_run = hash_json(world.input().required("migrationRun").unwrap()).unwrap();
        let original_call = hash_json(world.input().required("migrationCall").unwrap()).unwrap();
        let result = subject
            .publish(
                &doc(world.initial("operation")),
                &mut steps,
                Json::Object(vec![]),
            )
            .await;
        {
            let outcome = result.unwrap();
            assert_eq!(outcome.get("run"), Some(&pin(200, 1.0)));
            assert_eq!(rows.borrow().len(), 3);
            assert!(
                rows.borrow()
                    .iter()
                    .all(|(key, _, _, _)| key != "task-board/run")
            );
            assert!(world.client.replies.lock().unwrap().is_empty());
            let writes = rows.borrow();
            let route = &writes[2]
                .1
                .required("nativeRoutes")
                .unwrap()
                .as_array()
                .unwrap()[0];
            let selection = route.required("catalog").unwrap();
            let selected_plan = base.required("plan").unwrap();
            assert_eq!(
                selection.required("nativeVersion").unwrap(),
                &Json::from("0.163.0")
            );
            for key in ["nativeVersion", "catalogSourceHash"] {
                assert_eq!(selection.get(key), selected_plan.get(key));
            }
        }
        assert_eq!(
            hash_json(world.input().required("migrationRun").unwrap()).unwrap(),
            original_run
        );
        assert_eq!(
            hash_json(world.input().required("migrationCall").unwrap()).unwrap(),
            original_call
        );
        assert!(world.cleanups.borrow().is_empty());
        assert!(world.trace.lock().unwrap().iter().all(|event| {
            let parts = event.as_array().unwrap();
            parts.first() != Some(&"rpc".into())
                || parts.get(1) != Some(&"tools.call".into())
                || ["agent.status", "agent.read", "agent.operation"]
                    .iter()
                    .any(|method| f(&parts[2], "qualifiedName").is_str(method))
        }));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn unscoped_engine_change_refuses_absent_unknown_foreign_or_disagreeing_storage_receipts() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let group = data()
        .as_array()
        .unwrap()
        .iter()
        .find(|group| {
            f(f(group, "base").required("plan").unwrap(), "nativeVersion").is_str("0.159.2")
        })
        .unwrap();
    for fault in [
        "absent",
        "current-absence",
        "expired",
        "unknown",
        "foreign-owner",
        "foreign-engine",
        "foreign-backend",
        "different-evidence",
    ] {
        let (base, row, mut replies) = scenario(&catalogs, group, false, "upgrade");
        let frame = replies
            .iter_mut()
            .find(|frame| {
                f(frame, "method").is_str("tools.call")
                    && f(f(frame, "args"), "qualifiedName").is_str("agent.operation")
            })
            .unwrap();
        let mut receipt = frame.required("value").unwrap().clone();
        if ["absent", "current-absence", "expired"].contains(&fault) {
            frame.insert(
                "error",
                Json::object([
                    (
                        "code",
                        if fault == "expired" {
                            "operation_expired"
                        } else {
                            "not_found"
                        }
                        .into(),
                    ),
                    ("message", "Original receipt unavailable.".into()),
                    ("outcome", "not_executed".into()),
                    (
                        "details",
                        Json::object([
                            ("kind", "agent_operation_absent".into()),
                            (
                                "operationId",
                                receipt.required("operationId").unwrap().clone(),
                            ),
                            (
                                "serviceNodeId",
                                if fault == "current-absence" {
                                    Json::from("joint-agent")
                                } else {
                                    receipt.required("serviceNodeId").unwrap().clone()
                                },
                            ),
                            ("epoch", Json::Null),
                        ]),
                    ),
                ]),
            );
        } else {
            match fault {
                "unknown" => {
                    receipt.insert("phase", "outcome_unknown".into());
                    receipt.insert("reply", Json::Null);
                    receipt.insert("code", "native_connection_lost".into());
                }
                "foreign-owner" => {
                    receipt.insert("serviceNodeId", "foreign-agent".into());
                }
                "foreign-engine" => {
                    receipt.insert(
                        "nativeExecutableHash",
                        format!("sha256:{}", "f".repeat(64)).into(),
                    );
                }
                "foreign-backend" => {
                    receipt.insert(
                        "backend",
                        BackendIdentity::new(
                            "joint".into(),
                            NativeProvider::Codex,
                            "primary".into(),
                        )
                        .unwrap()
                        .json(),
                    );
                }
                "different-evidence" => {
                    receipt.insert("epoch", "another-original-epoch".into());
                }
                _ => unreachable!(),
            }
            frame.insert("value", receipt);
        }
        let trace: Trace = Arc::default();
        let client = Arc::new(Client {
            case_name: fault.into(),
            replies: Mutex::new(replies.into()),
            trace: trace.clone(),
        });
        let world = World {
            base: &base,
            row: &row,
            live: RefCell::new(
                row.required("initial")
                    .unwrap()
                    .required("liveOperation")
                    .unwrap()
                    .clone(),
            ),
            trace,
            tools: Arc::new(ToolClients::new(client.clone())),
            client,
            consumed: Cell::new(false),
            cleanups: RefCell::default(),
        };
        let original = hash_json(world.input()).unwrap();
        let subject = NativeLifecycle {
            store: &world,
            engine: &world,
            operations: &world,
            catalogs: &catalogs,
        };
        let rows = RefCell::new(vec![]);
        let mut steps = RetainedSteps {
            world: &world,
            rows: &rows,
            index: 0,
            lose: 0,
        };
        let error = subject
            .publish(
                &doc(world.initial("operation")),
                &mut steps,
                Json::Object(vec![]),
            )
            .await
            .unwrap_err();
        let expected = match fault {
            "absent" | "unknown" => "task_board_native_outcome_unresolved",
            "current-absence" => "native_absence_mismatch",
            "expired" => "operation_expired",
            "different-evidence" => "task_board_evidence_mismatch",
            _ => "native_evidence_mismatch",
        };
        assert_eq!(error.code, expected, "{fault}: {error:?}");
        assert!(rows.borrow().is_empty(), "{fault}");
        assert!(world.cleanups.borrow().is_empty());
        assert_eq!(hash_json(world.input()).unwrap(), original);
        assert!(world.trace.lock().unwrap().iter().all(|event| {
            let parts = event.as_array().unwrap();
            parts.first() != Some(&"rpc".into())
                || parts.get(1) != Some(&"tools.call".into())
                || ["agent.status", "agent.operation"]
                    .iter()
                    .any(|method| f(&parts[2], "qualifiedName").is_str(method))
        }));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn backend_binding_with_unchanged_service_and_namespace_preserves_and_deduplicates_contexts()
{
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    for group in data().as_array().unwrap().iter().filter(|group| {
        ["0.142.3", "0.163.0"].iter().any(|version| {
            f(f(group, "base").required("plan").unwrap(), "nativeVersion").is_str(version)
        })
    }) {
        let (base, row, replies) = scenario(&catalogs, group, false, "same-address");
        let trace: Trace = Arc::default();
        let client = Arc::new(Client {
            case_name: "same-address backend binding".into(),
            replies: Mutex::new(replies.into()),
            trace: trace.clone(),
        });
        let world = World {
            base: &base,
            row: &row,
            live: RefCell::new(
                row.required("initial")
                    .unwrap()
                    .required("liveOperation")
                    .unwrap()
                    .clone(),
            ),
            trace,
            tools: Arc::new(ToolClients::new(client.clone())),
            client,
            consumed: Cell::new(false),
            cleanups: RefCell::default(),
        };
        let original = world
            .input()
            .required("migrationRun")
            .unwrap()
            .required("value")
            .unwrap()
            .clone();
        let subject = NativeLifecycle {
            store: &world,
            engine: &world,
            operations: &world,
            catalogs: &catalogs,
        };
        let rows = RefCell::new(vec![]);
        let mut steps = RetainedSteps {
            world: &world,
            rows: &rows,
            index: 0,
            lose: 0,
        };
        subject
            .publish(
                &doc(world.initial("operation")),
                &mut steps,
                Json::Object(vec![]),
            )
            .await
            .unwrap();
        let writes = rows.borrow();
        let task = &writes[2].1;
        assert_eq!(
            task.get("primaryResourceRef"),
            world.task().value.get("primaryResourceRef")
        );
        assert_eq!(task.get("lastRun"), world.task().value.get("lastRun"));
        assert_eq!(
            task.required("primaryBackend")
                .unwrap()
                .required("backendId")
                .unwrap(),
            &"codex".into()
        );
        assert!(world.client.replies.lock().unwrap().is_empty());
        archive_migrated_history(&catalogs, group, task, &original, &original).await;
    }
}
