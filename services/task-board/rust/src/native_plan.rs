//! Resolve saved TaskBoard plans against their exact retained native catalog.
use crate::operations::Store;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, checked_native_contract::CheckedNativeContracts,
    native_backend::BackendIdentity, native_contract::NativeContract,
};
use ivy_sdk::native_evidence::NativeOwnerIdentity;
use ivy_sdk::native_plan::{NativePlanKind, read_native_plan};
use std::sync::Arc;

pub(crate) fn catalog(
    catalogs: &CheckedNativeContracts,
    value: &Json,
) -> IvyResult<Arc<NativeContract>> {
    if value.is_null() {
        return Err(IvyError::internal());
    }
    // Unsupported/missing versions must reach the shared version gate before
    // identity admission, like the original checkedNativeContract call.
    let version = value
        .get("nativeVersion")
        .and_then(Json::as_js_string)
        .cloned()
        .unwrap_or_default();
    let identity = Json::object([(
        "sourceHash",
        value
            .get("catalogSourceHash")
            .cloned()
            .unwrap_or(Json::Null),
    )]);
    let contract = catalogs.checked_native_contract(&version, Some(&identity))?;
    match BackendIdentity::optional(value.get("backend"))? {
        Some(backend) => Ok(Arc::new(contract.with_backend(Some(backend)))),
        None => Ok(contract),
    }
}

/// Current route and retained request must select the same explicit account.
pub(crate) fn owner(
    catalogs: &CheckedNativeContracts,
    target: &Json,
    selection: &Json,
) -> IvyResult<NativeOwnerIdentity> {
    let source = catalog(catalogs, selection)?;
    if !BackendIdentity::matches(target, source.backend()) {
        return Err(IvyError::new(
            "task_board_target_mismatch",
            "The target and plan must select the same backend and account.",
        ));
    }
    Ok(NativeOwnerIdentity {
        backend: source.backend().cloned(),
        service_node_id: target
            .required("serviceNodeId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?
            .clone(),
        host_id: target.get("hostId").and_then(Json::as_js_string).cloned(),
        native_version: selection
            .required("nativeVersion")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?
            .clone(),
        native_executable_hash: source
            .catalog()
            .required("nativeExecutableHash")?
            .as_js_string()
            .unwrap()
            .clone(),
        catalog_hash: source.catalog_hash().into(),
    })
}

pub(crate) fn matches_primary(target: &Json, primary: &Json) -> IvyResult<bool> {
    let backend = BackendIdentity::optional(target.get("backend"))?;
    Ok(primary
        .get("namespace")
        .is_some_and(|v| v.is_str(backend.as_ref().map_or("codex", BackendIdentity::id)))
        && primary.get("kind").is_some_and(|v| v.is_str("thread"))
        && primary.get("serviceNodeId") == target.get("serviceNodeId"))
}

pub(crate) fn matches_task_primary(target: &Json, task: &Json) -> IvyResult<bool> {
    Ok(
        matches_primary(target, task.required("primaryResourceRef")?)?
            && BackendIdentity::optional(task.get("primaryBackend"))?
                == BackendIdentity::optional(target.get("backend"))?,
    )
}

/// Explicitly published route changes affect current addresses only. Retained
/// requests, native owners, plans and evidence remain their original records.
pub(crate) fn routed_target(task: &Json, target: &Json) -> IvyResult<Json> {
    for route in routes(task)? {
        let source = route.required("source")?;
        if same_target(target, source)? {
            return Ok(route.required("target")?.clone());
        }
    }
    Ok(target.clone())
}

/// A published catalog selection applies only to current calls and reads.
/// A route without one still selects the Run's exact retained catalog.
pub(crate) fn routed_selection<'a>(task: &'a Json, run: &'a Json) -> IvyResult<&'a Json> {
    for route in routes(task)? {
        if same_target(run.required("target")?, route.required("source")?)? {
            return Ok(route.get("catalog").unwrap_or(run.required("plan")?));
        }
    }
    run.required("plan")
}

/// Retained calls keep their original owner and catalog. Current work uses
/// only the catalog explicitly admitted with the published destination.
pub(crate) fn routed_owner(
    catalogs: &CheckedNativeContracts,
    task: &Json,
    run: &Json,
) -> IvyResult<NativeOwnerIdentity> {
    let original = run.required("target")?;
    let mut selected = owner(catalogs, original, run.required("plan")?)?;
    let target = routed_target(task, original)?;
    selected.backend = BackendIdentity::optional(target.get("backend"))?;
    selected.service_node_id = target
        .required("serviceNodeId")?
        .as_js_string()
        .ok_or_else(IvyError::internal)?
        .clone();
    selected.host_id = target.get("hostId").and_then(Json::as_js_string).cloned();
    let selection = routed_selection(task, run)?;
    if selection != run.required("plan")? {
        let source = catalog(catalogs, selection)?.with_backend(selected.backend.clone());
        selected.native_version = selection.required("nativeVersion")?.as_js_string()
            .ok_or_else(IvyError::internal)?.clone();
        selected.native_executable_hash = source.catalog().required("nativeExecutableHash")?
            .as_js_string().ok_or_else(IvyError::internal)?.clone();
        selected.catalog_hash = source.catalog_hash().into();
    }
    Ok(selected)
}

/// Retained reads may name a previous published owner. Every old source still
/// maps directly to the current destination after successive moves.
pub(crate) fn read_target_matches(
    task: &Json,
    original: &Json,
    service: &ivy_contracts::JsString,
    backend: Option<&BackendIdentity>,
    retained: bool,
) -> IvyResult<bool> {
    let matches = |target: &Json| -> IvyResult<bool> {
        Ok(
            target.get("serviceNodeId").and_then(Json::as_js_string) == Some(service)
                && BackendIdentity::optional(target.get("backend"))?.as_ref() == backend,
        )
    };
    let current = routed_target(task, original)?;
    if matches(&current)? {
        return Ok(true);
    }
    if retained && !same_target(&current, original)? {
        for route in routes(task)? {
            if same_target(route.required("target")?, &current)?
                && matches(route.required("source")?)?
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn routes(task: &Json) -> IvyResult<&[Json]> {
    match task.get("nativeRoutes") {
        None => Ok(&[]),
        Some(Json::Array(routes)) if routes.len() <= 64 => Ok(routes),
        _ => Err(IvyError::internal()),
    }
}

fn same_target(left: &Json, right: &Json) -> IvyResult<bool> {
    Ok(left.get("serviceNodeId") == right.get("serviceNodeId")
        && left.get("hostId") == right.get("hostId")
        && BackendIdentity::optional(left.get("backend"))?
            == BackendIdentity::optional(right.get("backend"))?)
}

/// Store direct current destinations, never a chain to traverse on each read.
pub(crate) fn moved_routes(task: &Json, source: &Json, target: &Json, plan: &Json) -> IvyResult<Json> {
    let selection = Json::object([
        ("nativeVersion", plan.required("nativeVersion")?.clone()),
        ("catalogSourceHash", plan.required("catalogSourceHash")?.clone()),
    ]);
    let mut moved = Vec::new();
    for route in routes(task)? {
        if same_target(route.required("source")?, target)? {
            return Err(IvyError::new(
                "task_board_target_mismatch",
                "A route move cannot return to an original retired owner.",
            ));
        }
        let mut route = route.clone();
        if same_target(route.required("target")?, source)? {
            route.insert("target", target.clone());
            route.insert("catalog", selection.clone());
        }
        moved.push(route);
    }
    if moved.len() >= 64 {
        return Err(IvyError::new(
            "limit_exceeded",
            "A ticket supports at most 64 original native owners.",
        ));
    }
    moved.push(Json::object([
        ("source", source.clone()),
        ("target", target.clone()),
        ("catalog", selection),
    ]));
    Ok(Json::Array(moved))
}

pub(crate) fn primary_route<'a>(
    task: &'a Json,
    primary: &Json,
    backend: Option<&BackendIdentity>,
) -> IvyResult<Option<&'a Json>> {
    for route in routes(task)? {
        let source = route.required("source")?;
        if primary.get("serviceNodeId") == source.get("serviceNodeId")
            && backend == BackendIdentity::optional(source.get("backend"))?.as_ref()
        {
            if !matches_primary(source, primary)? {
                return Err(IvyError::new(
                    "task_board_primary_mismatch",
                    "The migrated resource must retain its original native namespace.",
                ));
            }
            return Ok(Some(route.required("target")?));
        }
    }
    Ok(None)
}

/// Readdress only current workspace references. The saved Run and its observed
/// paths, project identifier and repository state remain the original record.
pub(crate) fn moved_workspace(workspace: &Json, source: &Json, target: &Json) -> IvyResult<Json> {
    let previous_backend = BackendIdentity::optional(source.get("backend"))?;
    let backend = BackendIdentity::from_json(target.required("backend")?)?;
    if workspace.get("hostId") != source.get("hostId")
        || workspace.get("serviceNodeId") != source.get("serviceNodeId")
        || source.get("hostId") != target.get("hostId")
    {
        return Err(IvyError::new(
            "task_board_workspace_mismatch",
            "A route move must retain the workspace's recorded physical host and owner.",
        ));
    }
    let mut workspace = workspace.clone();
    workspace.insert("serviceNodeId", target.required("serviceNodeId")?.clone());
    if let Some(project) = workspace.get("project").filter(|v| !v.is_null()) {
        if project.get("serviceNodeId") != source.get("serviceNodeId") {
            return Err(IvyError::new(
                "task_board_workspace_mismatch",
                "The original project must belong to its recorded workspace owner.",
            ));
        }
        let mut project = project.clone();
        project.insert("serviceNodeId", target.required("serviceNodeId")?.clone());
        if project.get("namespace").is_some_and(|v| {
            v.is_str(
                previous_backend
                    .as_ref()
                    .map_or("codex", BackendIdentity::id),
            )
        }) {
            project.insert("namespace", backend.id().into());
        }
        workspace.insert("project", project);
    }
    Ok(workspace)
}

pub(crate) fn routed_workspace(task: &Json, target: &Json, workspace: &Json) -> IvyResult<Json> {
    let current = routed_target(task, target)?;
    if &current == target {
        Ok(workspace.clone())
    } else {
        moved_workspace(workspace, target, &current)
    }
}

pub(crate) fn routed_primary(
    task: &Json,
    primary: &Json,
    backend: Option<BackendIdentity>,
) -> IvyResult<(Json, Option<BackendIdentity>)> {
    if let Some(target) = primary_route(task, primary, backend.as_ref())? {
        let selected = BackendIdentity::from_json(target.required("backend")?)?;
        let mut current = primary.clone();
        current.insert("serviceNodeId", target.required("serviceNodeId")?.clone());
        current.insert("namespace", selected.id().into());
        return Ok((current, Some(selected)));
    }
    Ok((primary.clone(), backend))
}

pub(crate) fn call_owner(
    catalogs: &CheckedNativeContracts,
    run: &Json,
    call: &Json,
) -> IvyResult<NativeOwnerIdentity> {
    let request = call.required("request")?;
    if let Some(saved) = call.get("owner") {
        let saved = NativeOwnerIdentity::from_json(saved)?;
        let source = catalog(catalogs, request)?;
        if !BackendIdentity::matches(request, saved.backend.as_ref())
            || request.get("nativeVersion").and_then(Json::as_js_string)
                != Some(&saved.native_version)
            || source
                .catalog()
                .get("nativeExecutableHash")
                .and_then(Json::as_js_string)
                != Some(&saved.native_executable_hash)
            || source.catalog_hash() != saved.catalog_hash.to_utf8()?
        {
            return Err(IvyError::new(
                "task_board_evidence_mismatch",
                "The call must retain its original backend and exact native catalog owner.",
            ));
        }
        return Ok(saved);
    }
    if request.get("backend").is_some() {
        return Err(IvyError::new(
            "task_board_evidence_mismatch",
            "A scoped call requires its saved original owner.",
        ));
    }
    let target = run
        .get("originalRequest")
        .and_then(|r| r.get("target"))
        .unwrap_or(run.required("target")?);
    owner(catalogs, target, request)
}

pub fn resolved_plan<S: Store>(
    store: &S,
    catalogs: &CheckedNativeContracts,
    plan: &Json,
) -> IvyResult<Json> {
    let contract = catalog(catalogs, plan)?;
    read_native_plan(
        &contract,
        plan,
        NativePlanKind::TaskBoard,
        store.root_object_id().as_ref(),
    )
}

pub async fn read_plan<S: Store>(
    store: &S,
    catalogs: &CheckedNativeContracts,
    pin: &Json,
) -> IvyResult<Json> {
    let plan = store.read("task-board/native-plan", pin, true).await?;
    resolved_plan(store, catalogs, &plan.value)
}
