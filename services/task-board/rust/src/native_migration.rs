//! A route move is a retained domain publication, never a native redispatch.
use super::*;
use crate::{configuration::effective_execution, native_intent::EvidenceIdentity, native_plan};
use ivy_sdk::native_parameters::read_native_parameters;

impl<S: LifecycleStore, E: LifecycleEngine, O: LifecycleOperations> NativeLifecycle<'_, S, E, O> {
    async fn collected_thread_receipt(
        &self,
        request: &Json,
        run: &Json,
        selected: &NativeOwner,
        original: &NativeOwnerIdentity,
    ) -> IvyResult<(
        Arc<ivy_contracts::native_contract::NativeContract>,
        JsString,
        Json,
    )> {
        // This ID is a lookup hint. Only the selected storage's authenticated
        // original receipt proves provenance; no missing local call is recreated.
        let operation_id = s(request, "originalThreadOperationId")?.clone();
        let observed = selected
            .management(
                "operation",
                Json::object([("operationId", operation_id.clone().into())]),
                None,
            )
            .await?;
        validate_agent("Operation", &observed)?;
        let method = s(&observed, "method")?.clone();
        let contract = native_plan::catalog(self.catalogs, run.required("plan")?)?;
        require(
            ["thread/start", "thread/resume"]
                .iter()
                .any(|v| method == JsString::from(*v))
                && observed.get("epoch") == run.get("nativeEpoch"),
            "task_board_evidence_mismatch",
            "The retained thread receipt must match the original Run epoch and thread method.",
        )?;
        let params = observed.required("params")?;
        contract.validate_input(&method, params, MANAGEMENT_FRAME_BYTES)?;
        selected
            .original_operations(original.clone())
            .check_operation(
                &observed,
                &NativeOperationCall {
                    operation_id,
                    method: method.clone(),
                    params: params.clone(),
                    definition_hash: contract.definition_hash(&method)?.into(),
                },
            )?;
        require(
            method != JsString::from("thread/resume")
                || params.get("threadId") == run.required("primaryResourceRef")?.get("nativeId"),
            "task_board_primary_mismatch",
            "The retained resume must identify the original durable thread.",
        )?;
        Ok((contract, method, observed))
    }

    pub(super) async fn migrate_route<P: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut P,
        outcome: &mut Json,
        task: &Document,
        current_archived: bool,
    ) -> IvyResult<()> {
        let request = operation.value.required("request")?;
        let source = self
            .store
            .read("task-board/run", request.required("run")?, true)
            .await?;
        let first = array(field(&operation.value, "writes")).is_empty();
        // Archive membership is mutable independently of the content revision.
        // Resume the exact saved condition, never the latest observed membership.
        let archived = if first {
            current_archived
        } else {
            array(operation.value.required("writes")?)[0]
                .required("destination")?
                .required("expectedArchived")?
                .as_bool()
                .ok_or_else(IvyError::internal)?
        };
        require(
            field(&task.value, "claim").is_null()
                && same(&source.pin.to_json(), request.required("run")?)?
                && source.value.get("taskId").and_then(Json::as_js_string)
                    == Some(&task.pin.object_id)
                && ["completed", "failed", "cancelled"]
                    .iter()
                    .any(|v| eq(&source.value, "phase", v))
                && !["unknown", "active"]
                    .iter()
                    .any(|v| eq(field(&source.value, "externalOutcome"), "state", v)),
            "task_board_route_migration_unresolved",
            "Route migration requires an exact terminal original attempt with a resolved outcome and no active claim.",
        )?;
        let target = request.required("target")?;
        let original_target = source.value.required("target")?;
        let old_target = native_plan::routed_target(&task.value, original_target)?;
        let backend = BackendIdentity::from_json(target.required("backend")?)?;
        let previous_backend = BackendIdentity::optional(old_target.get("backend"))?;
        require(
            target.get("hostId") == old_target.get("hostId")
                && !same(target, &old_target)?
                && previous_backend.as_ref().is_none_or(|old| {
                    old.provider() == backend.provider() && old.account_id() == backend.account_id()
                }),
            "task_board_target_mismatch",
            "Consolidation must retain the physical host, provider and account; a provider or account change requires a new context.",
        )?;
        let moves_primary = !field(&task.value, "primaryResourceRef").is_null()
            && native_plan::matches_task_primary(&old_target, &task.value)?;
        let (original_primary, _) = native_plan::routed_primary(
            &task.value,
            source.value.required("primaryResourceRef")?,
            BackendIdentity::optional(original_target.get("backend"))?,
        )?;
        require(
            !moves_primary
                || (same(&source.pin.to_json(), task.value.required("lastRun")?)?
                    && same(
                        &original_primary,
                        task.value.required("primaryResourceRef")?,
                    )?),
            "task_board_route_migration_unresolved",
            "Moving the current primary requires its exact current attempt and unchanged thread.",
        )?;
        let plan = read_plan(self.store, self.catalogs, request.required("intent")?).await?;
        let old_plan = source.value.required("plan")?;
        for name in ["threadStart", "threadResume", "turnStart"] {
            require(
                same(field(old_plan, name), field(&plan, name))?,
                "task_board_native_intent_mismatch",
                "A route move must preserve all original plan parameters. Prepare workspace or execution-option changes separately.",
            )?;
        }
        if let Some(location) = plan.get("location").filter(|v| !v.is_null()) {
            require(
                location.get("hostId") == target.get("hostId")
                    && location.get("serviceNodeId") == target.get("serviceNodeId"),
                "task_board_target_mismatch",
                "The new plan location must identify the selected route.",
            )?;
        }
        let expected_owner = native_plan::owner(self.catalogs, target, &plan)?;
        let routes = native_plan::moved_routes(&task.value, &old_target, target, &plan)?;
        let original_owner = native_plan::owner(self.catalogs, original_target, old_plan)?;
        let needs_storage_receipt = previous_backend.is_none()
            && !(original_owner.native_version == expected_owner.native_version
                && original_owner.native_executable_hash == expected_owner.native_executable_hash
                && old_plan.get("catalogSourceHash") == plan.get("catalogSourceHash"));
        let workspace = native_plan::moved_workspace(
            source.value.required("workspace")?,
            original_target,
            target,
        )?;
        require(
            same(&workspace, request.required("workspace")?)?,
            "task_board_workspace_mismatch",
            "A route move preserves every workspace path, project ID, repository observation and worktree setting.",
        )?;
        let mut primary = source.value.required("primaryResourceRef")?.clone();
        primary.insert("serviceNodeId", target.required("serviceNodeId")?.clone());
        primary.insert("namespace", backend.id().into());
        let options = if moves_primary {
            let mut options = effective_execution(self.store, &task.value)
                .await?
                .required("nativeOptions")?
                .clone();
            require(
                BackendIdentity::optional(options.get("backend"))? == previous_backend,
                "task_board_target_mismatch",
                "Apply pending backend selection changes separately before migrating the current context.",
            )?;
            options.insert("backend", backend.json());
            Some(options)
        } else {
            None
        };

        if first {
            self.store
                .migration_quiescent(&task.pin.object_id, &operation.pin.object_id)?;
            let current_run = self
                .store
                .read(
                    "task-board/run",
                    &Json::String(source.pin.object_id.clone()),
                    true,
                )
                .await?;
            require(
                current_run.pin == source.pin,
                "revision_conflict",
                "Reload the source attempt before migrating its route.",
            )?;
            // Checking every attempt also refuses an older unresolved Run. No
            // context is archived, resumed, interrupted or unloaded by this API.
            self.contexts(task, false).await?;
            self.engine.verify_owner().await?;
            let selected = self
                .owner(s(target, "serviceNodeId")?, Some(&backend))
                .await?;
            require(
                selected.target() == &expected_owner,
                "task_board_native_epoch_changed",
                "The selected live native owner must match the exact new plan catalog.",
            )?;
            let call_address = source.value.required("calls")?.required("thread")?;
            let call = if call_address.is_null() {
                None
            } else {
                match self
                    .store
                    .read("task-board/native-call", call_address, true)
                    .await
                {
                    Ok(call) => Some(call),
                    Err(error)
                        if error.outcome == ivy_contracts::Outcome::NotExecuted
                            && (error.code == "not_found" || error.code == "revision_pruned") =>
                    {
                        None
                    }
                    Err(error) => return Err(error),
                }
            };
            let receipt = if let Some(call) = call {
                require(
                    request
                        .get("originalThreadOperationId")
                        .is_none_or(|id| call.value.get("operationId") == Some(id)),
                    "task_board_evidence_mismatch",
                    "The supplied receipt ID must match the surviving original call.",
                )?;
                let original = native_plan::call_owner(self.catalogs, &source.value, &call.value)?;
                let native_request = call.value.required("request")?;
                let contract = native_plan::catalog(self.catalogs, native_request)?;
                require(
                    ["thread/start", "thread/resume"]
                        .iter()
                        .any(|m| eq(native_request, "method", m))
                        && eq(&call.value, "ownerPhase", "succeeded")
                        && original == original_owner,
                    "task_board_evidence_mismatch",
                    "The move requires the original successful thread call from the Run's exact retained native owner.",
                )?;
                let method = s(native_request, "method")?.clone();
                require(
                    s(&call.value, "expectedDefinitionHash")?.to_utf8()?
                        == contract.definition_hash(&method)?,
                    "task_board_evidence_mismatch",
                    "The original call definition must belong to its retained catalog.",
                )?;
                let params = read_native_parameters(
                    &contract,
                    &method,
                    native_request.required("params")?,
                    &[],
                    self.store.root_object_id().as_ref(),
                )?;
                // Prefer the domain's exact pinned evidence. Its lifetime is not the
                // manager's receipt-cache lifetime; missing/expired owner receipts
                // never authorize replay or a fabricated successful observation.
                // An old unscoped catalog does not identify its provider/account.
                // For an engine change, require the selected backend's retained
                // original receipt as storage provenance, in addition to any pinned
                // domain evidence. The manager binds that storage's original
                // provider before admitting its explicit account. Version/provider
                // labels in a protocol catalog cannot establish that ownership.
                let mut storage_receipt = if needs_storage_receipt
                    || field(&call.value, "evidence").is_null()
                {
                    Some(
                        selected
                            .original_operations(original.clone())
                            .operation(&NativeOperationCall {
                                operation_id: s(&call.value, "operationId")?.clone(),
                                method: method.clone(),
                                params: params.clone(),
                                definition_hash: s(&call.value, "expectedDefinitionHash")?.clone(),
                            })
                            .await?,
                    )
                } else {
                    None
                };
                if needs_storage_receipt {
                    require(
                        storage_receipt
                            .as_ref()
                            .is_some_and(|receipt| eq(receipt, "phase", "succeeded")),
                        "task_board_native_outcome_unresolved",
                        "An unscoped engine change requires the selected storage's exact successful original receipt; absence or uncertainty never proves ownership.",
                    )?;
                }
                // Release the complete storage reply before the pinned reader
                // materializes another potentially large original thread reply.
                // The shared hasher streams the same bounded canonical JSON.
                let storage_hash = if !field(&call.value, "evidence").is_null() {
                    storage_receipt
                        .take()
                        .map(|receipt| hash_json(&receipt))
                        .transpose()?
                } else {
                    None
                };
                let observed = if field(&call.value, "evidence").is_null() {
                    storage_receipt.ok_or_else(IvyError::internal)?
                } else {
                    let observed = self
                        .store
                        .migration_evidence(
                            self.catalogs,
                            &call,
                            &EvidenceIdentity {
                                backend: original.backend,
                                operation_id: s(&call.value, "operationId")?.clone(),
                                caller_principal_id: self.engine.principal_id().clone(),
                                service_node_id: original.service_node_id,
                                native_version: original.native_version,
                                method: method.clone(),
                                params,
                            },
                        )
                        .await?;
                    if let Some(hash) = storage_hash {
                        require(
                            hash == hash_json(&observed)?,
                            "task_board_evidence_mismatch",
                            "The selected storage and pinned original evidence must identify the same receipt.",
                        )?;
                    }
                    observed
                };
                Some((contract, method, observed))
            } else if request.get("originalThreadOperationId").is_some() {
                Some(
                    self.collected_thread_receipt(
                        request,
                        &source.value,
                        &selected,
                        &original_owner,
                    )
                    .await?,
                )
            } else {
                // A terminal Run outlives its collected call graph. The selected
                // manager's persisted origin authenticates the retained storage;
                // the fresh native read below identifies the durable thread.
                let origin = selected
                    .management("storageIdentity", Json::object::<&str>([]), None)
                    .await?;
                validate_agent("StorageIdentity", &origin)?;
                require(
                    origin.get("serviceNodeId") == target.get("serviceNodeId")
                        && origin.get("originalServiceNodeId")
                            == Some(&original_owner.service_node_id.clone().into())
                        && origin.get("provider") == Some(&backend.provider().as_str().into())
                        && BackendIdentity::matches(&origin, Some(&backend)),
                    "task_board_evidence_mismatch",
                    "The selected retained storage must preserve the original service, provider and selected account.",
                )?;
                None
            };
            if let Some((contract, method, observed)) = receipt {
                require(
                    eq(&observed, "phase", "succeeded"),
                    "task_board_native_outcome_unresolved",
                    "Only a retained successful original thread observation can prove the moved storage; absence never authorizes replay.",
                )?;
                let result = observed.required("reply")?.required("result")?;
                contract.validate_result(&method, result, MANAGEMENT_FRAME_BYTES)?;
                require(
                    field(field(result, "thread"), "id") == primary.required("nativeId")?,
                    "task_board_primary_mismatch",
                    "The original receipt must identify this exact durable thread.",
                )?;
            }
            let read = selected
                .read(
                    &"thread/read".into(),
                    Json::object([
                        ("threadId", primary.required("nativeId")?.clone()),
                        ("includeTurns", false.into()),
                    ]),
                    None,
                    false,
                )
                .await?;
            native_plan::catalog(self.catalogs, &plan)?.validate_result(
                &"thread/read".into(),
                read.required("reply")?.required("result")?,
                MANAGEMENT_FRAME_BYTES,
            )?;
            let thread = field(field(field(&read, "reply"), "result"), "thread");
            require(
                thread.get("id") == primary.get("nativeId")
                    && thread.get("ephemeral") == Some(&false.into())
                    && !eq(field(thread, "status"), "type", "active")
                    && thread.get("cwd") == workspace.get("canonicalCwd")
                    && (field(&workspace, "project").is_null()
                        || thread.get("projectId") == field(&workspace, "project").get("nativeId")),
                "task_board_primary_mismatch",
                "The selected route must return the same durable, idle thread and unchanged workspace.",
            )?;
        }

        let at = operation.value.required("createdAt")?;
        let mut next = task.value.clone();
        next.insert("publication", steps.pin()?);
        next.insert("updatedAt", at.clone());
        let reserved = write_header(
            steps,
            &next,
            &Json::object([
                ("objectId", Json::String(task.pin.object_id.clone())),
                ("expectedRevision", Json::Number(task.pin.revision)),
                ("expectedArchived", archived.into()),
            ]),
        )
        .await?;
        let history = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("taskId", Json::String(task.pin.object_id.clone())),
            (
                "operationId",
                operation.value.required("operationId")?.clone(),
            ),
            ("actor", operation.value.required("actor")?.clone()),
            ("createdAt", at.clone()),
            ("kind", "recovered".into()),
            (
                "detail",
                Json::String(join(
                    "Moved the current native route without dispatching native work. ",
                    s(request, "reason")?,
                )),
            ),
            ("previousTask", task.pin.to_json()),
            (
                "previousHistory",
                task.value.required("lastHistory")?.clone(),
            ),
            ("fromTarget", old_target),
            ("toTarget", target.clone()),
            ("priorRun", source.pin.to_json()),
            ("result", Json::Null),
            ("review", Json::Null),
        ]);
        let history = steps
            .create("task-board/history", &history, &"History".into())
            .await?;
        if let Some(options) = options {
            next.insert("primaryResourceRef", primary);
            next.insert("primaryBackend", backend.json());
            let mut fields = next.required("fields")?.clone();
            fields.insert("nativeOptions", options);
            next.insert("fields", fields);
        }
        next.insert("nativeRoutes", routes);
        next.insert("lastHistory", history.clone());
        next.insert("publication", Json::Null);
        let task = write_header(
            steps,
            &next,
            &Json::object([
                ("objectId", Json::String(task.pin.object_id.clone())),
                ("expectedRevision", reserved.required("revision")?.clone()),
                ("expectedArchived", archived.into()),
            ]),
        )
        .await?;
        outcome.insert("task", task);
        outcome.insert("run", source.pin.to_json());
        outcome.insert("history", history);
        Ok(())
    }
}
