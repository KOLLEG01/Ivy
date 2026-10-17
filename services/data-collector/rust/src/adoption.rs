//! Local adoption commit after the control plane has fetched and validated exact
//! definition/script/assignment pins and reconciled disabled worker ownership.
use crate::{cache::ExecutionCache, model, store::CollectorStore};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};

pub struct Adoption<'a> {
    pub task: &'a IvyJson,
    pub desired: &'a IvyJson,
    pub assignment_id: &'a str,
    pub assignment_revision: f64,
    pub service_node_id: &'a str,
    pub settings: &'a IvyJson,
    pub confirmed_stopped: bool,
    pub ever_accepted: bool,
    pub active: bool,
    pub now_ms: f64,
}
pub fn adopt(store: &CollectorStore, input: Adoption<'_>) -> IvyResult<bool> {
    require(
        model::text(input.desired, "serviceNodeId")? == input.service_node_id,
        "target_conflict",
        "Only the assigned instance can adopt an execution definition.",
    )?;
    let job = model::text(input.desired, "jobId")?;
    let id = model::text(input.task, "id")?;
    let existing = store.get(&id, true)?;
    if let Some(row) = &existing {
        if let Some(definition) = row.get("definition") {
            require(
                model::text(definition, "jobId")? == job || model::truth(row, "deleted"),
                "target_conflict",
                "Another job already owns this local task ID.",
            )?;
        }
    }
    if input.active || store.executing(&id)? {
        return Ok(false);
    }
    let cache = ExecutionCache { store };
    if let Some(row) = &existing {
        require(
            !cache.unconfirmed(row)?,
            "outcome_unknown",
            "Verify the interrupted worker before accepting another execution definition.",
        )?;
    }
    if !input.confirmed_stopped
        && existing
            .as_ref()
            .and_then(|r| r.get("definition"))
            .is_none()
    {
        require(
            !input.ever_accepted,
            "recovery_required",
            "The accepted cache was lost. Verify old workers and explicitly recover before resuming.",
        )?;
    }
    let deleted = model::truth(input.desired, "deleted");
    let mut task = input.task.clone();
    task.insert(
        "enabled",
        (model::truth(input.desired, "enabled") && !deleted).into(),
    );
    if model::truth(&task, "enabled") {
        let IvyJson::Array(names) = model::field(&task, "secretNames")? else {
            return Err(IvyError::new("invalid_arguments", "Invalid secret names."));
        };
        let mut names = names.clone();
        if let Some(name) = model::optional_text(&task, "inputSecretName").filter(|s| !s.is_empty())
        {
            names.push(name.into());
        }
        let secrets = model::field(input.settings, "secrets")?;
        for name in names {
            require(
                secrets.get(&name.as_str()?).is_some(),
                "secret_missing",
                "An assigned secret is unavailable on this instance.",
            )?;
        }
        require(
            model::number(&task, "memoryMb")? <= model::number(input.settings, "maximumMemoryMb")?
                && model::number(&task, "timeoutSeconds")?
                    <= model::number(input.settings, "maximumTimeoutSeconds")?,
            "limit_exceeded",
            "Execution requirements exceed this instance's limits.",
        )?;
    }
    store.transaction(|store| {
        let current = store.get(&id, true)?;
        // DC01: a deleted row no longer occupies an active slot. Count and save
        // occur under the same writer transaction, including deleted-ID reuse.
        require(
            current
                .as_ref()
                .is_some_and(|r| !model::truth(r, "deleted"))
                || (store.list(false)?.len() as f64)
                    < model::number(input.settings, "maximumTasks")?,
            "limit_exceeded",
            "Maximum task count reached on the assigned instance.",
        )?;
        let staged = store.metadata(&format!("handoff-state:{job}"))?;
        let handoff = if store
            .metadata(&format!("handoff-consumed:{job}"))?
            .as_deref()
            != Some("true")
        {
            staged.map(|s| parse(&s, usize::MAX)).transpose()?
        } else {
            None
        };
        let transfer = handoff.as_ref().filter(|h| {
            model::optional_text(h, "assignmentId").as_deref() == Some(input.assignment_id)
                && h.get("assignmentRevision").and_then(IvyJson::as_f64)
                    == Some(input.assignment_revision)
        });
        let changed = match &current {
            None => true,
            Some(row) => {
                canonical(model::field(row, "task")?, MANAGEMENT_FRAME_BYTES)?
                    != canonical(&task, MANAGEMENT_FRAME_BYTES)?
            }
        };
        let previous = |key: &str| {
            current
                .as_ref()
                .and_then(|r| r.get(key))
                .filter(|v| **v != IvyJson::Null)
                .cloned()
        };
        let state = if deleted {
            IvyJson::Null
        } else if let Some(transfer) = transfer {
            model::field(transfer, "state")?.clone()
        } else {
            previous("state").unwrap_or(IvyJson::Null)
        };
        let next_at = if let Some(transfer) = transfer {
            model::field(transfer, "nextAt")?.clone()
        } else if !changed && current.is_some() {
            previous("nextAt").unwrap_or(IvyJson::Null)
        } else {
            IvyJson::Number(input.now_ms + model::number(&task, "intervalSeconds")? * 1000.0)
        };
        if deleted {
            task.insert("enabled", false.into());
            task.insert("script", "".into());
            task.insert("config", model::object());
            task.insert("dependencies", model::object());
            task.insert("secretNames", IvyJson::Array(Vec::new()));
            task.insert("inputSecretName", IvyJson::Null);
        }
        let row = IvyJson::object([
            ("task", task),
            (
                "revision",
                IvyJson::Number(
                    previous("revision").and_then(|v| v.as_f64()).unwrap_or(0.0)
                        + if changed { 1.0 } else { 0.0 },
                ),
            ),
            ("state", state),
            ("nextAt", next_at),
            (
                "resultObjectId",
                input
                    .desired
                    .get("resultObjectId")
                    .filter(|v| **v != IvyJson::Null)
                    .cloned()
                    .or_else(|| previous("resultObjectId"))
                    .unwrap_or(IvyJson::Null),
            ),
            (
                "resultRootId",
                model::field(input.desired, "rootObjectId")?.clone(),
            ),
            ("lastRunId", previous("lastRunId").unwrap_or(IvyJson::Null)),
            (
                "retentionWarning",
                previous("retentionWarning").unwrap_or(IvyJson::Null),
            ),
            ("deleted", deleted.into()),
            (
                "definition",
                IvyJson::object([
                    ("jobId", job.clone().into()),
                    (
                        "revision",
                        model::field(input.desired, "definitionRevision")?.clone(),
                    ),
                    ("assignmentId", input.assignment_id.into()),
                    (
                        "assignmentRevision",
                        IvyJson::Number(input.assignment_revision),
                    ),
                ]),
            ),
            ("adoptionError", IvyJson::Null),
        ]);
        store.save(&row)?;
        if transfer.is_some() {
            store.set_metadata(&format!("handoff-consumed:{job}"), "true")?;
        }
        let frozen = store
            .metadata(&format!("handoff-lock:{job}"))?
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        if input.assignment_revision > frozen {
            store.set_metadata(&format!("handoff-lock:{job}"), "0")?;
        }
        Ok(true)
    })
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
