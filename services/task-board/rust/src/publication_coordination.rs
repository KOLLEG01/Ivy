//! Durable root/task publication gates over the original retained journal.
use crate::{
    journal_store::{Contracts, JournalStore},
    lifecycle_journal::LifecycleJournal,
    local_gates::LocalGates,
    operations::{Document, Store},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    hash::hash_json,
    json::{canonical, js_string},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::RpcClient;
use ivy_store::sqlite::error;
#[allow(async_fn_in_trait)]
pub trait CoordinationStore: Store {
    fn operation_gates(&self, operation: &JsString) -> IvyResult<Vec<JsString>>;
    fn retain_operation_gates(&self, operation: &JsString, names: &[JsString]) -> IvyResult<()>;
    fn pending_coordination(&self) -> IvyResult<Option<Json>>;
    async fn named_gate(&self, name: &JsString) -> IvyResult<Option<Document>>;
}
impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString> CoordinationStore
    for JournalStore<'_, R, P, C, I>
{
    fn operation_gates(&self, id: &JsString) -> IvyResult<Vec<JsString>> {
        LocalGates::new(self.journal.connection)
            .operation_gates(id)
            .map_err(error)
    }
    fn retain_operation_gates(&self, id: &JsString, names: &[JsString]) -> IvyResult<()> {
        LocalGates::new(self.journal.connection)
            .retain_operation_gates(id, names)
            .map_err(error)
    }
    fn pending_coordination(&self) -> IvyResult<Option<Json>> {
        Ok(LocalGates::new(self.journal.connection)
            .pending_coordination()
            .map_err(error)?
            .map(|id| {
                Json::object([
                    ("objectId", Json::String(id)),
                    ("revision", Json::Number(1.0)),
                ])
            }))
    }
    async fn named_gate(&self, name: &JsString) -> IvyResult<Option<Document>> {
        self.named_record(
            "task-board/coordination",
            name,
            self.journal.root_object_id.as_ref(),
        )
        .await
    }
}
impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString, Q>
    CoordinationStore for LifecycleJournal<'_, '_, R, P, C, I, Q>
{
    fn operation_gates(&self, id: &JsString) -> IvyResult<Vec<JsString>> {
        self.journal.operation_gates(id)
    }
    fn retain_operation_gates(&self, id: &JsString, names: &[JsString]) -> IvyResult<()> {
        self.journal.retain_operation_gates(id, names)
    }
    fn pending_coordination(&self) -> IvyResult<Option<Json>> {
        self.journal.pending_coordination()
    }
    async fn named_gate(&self, name: &JsString) -> IvyResult<Option<Document>> {
        self.journal.named_gate(name).await
    }
}
pub struct PublicationCoordination<'a, S, C> {
    pub store: &'a S,
    pub principal_id: &'a JsString,
    pub now: &'a C,
}
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn truthy(v: &Json) -> bool {
    match v {
        Json::Null => false,
        Json::Bool(v) => *v,
        Json::Number(v) => *v != 0.0 && !v.is_nan(),
        Json::String(v) => !v.0.is_empty(),
        _ => true,
    }
}
fn eq(v: &Json, k: &str, t: &str) -> bool {
    v.get(k) == Some(&Json::from(t))
}
fn same(a: &Json, b: &Json) -> IvyResult<bool> {
    Ok(canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn concat(parts: impl IntoIterator<Item = JsString>) -> JsString {
    JsString(parts.into_iter().flat_map(|s| s.0).collect())
}
fn mutation(scope: &JsString, step: JsString) -> IvyResult<String> {
    Ok(format!(
        "task-board:{}",
        &hash_json(&Json::object([
            ("scope", Json::String(scope.clone())),
            ("step", Json::String(step))
        ]))?[7..]
    ))
}
fn finished(v: &Json) -> bool {
    eq(v, "phase", "succeeded") || eq(v, "phase", "failed")
}
impl<S: CoordinationStore, C: Fn() -> String> PublicationCoordination<'_, S, C> {
    async fn gates(&self, operation: &Document) -> IvyResult<Vec<JsString>> {
        let id = s(&operation.pin, "objectId")?;
        let retained = self.store.operation_gates(id)?;
        if !retained.is_empty() {
            return Ok(retained);
        }
        let request = f(&operation.value, "request");
        let names = if request.get("taskId").is_none() {
            vec!["TaskBoard publication gate".into()]
        } else {
            let task = concat([
                "TaskBoard publication gate ".into(),
                s(request, "taskId")?.clone(),
            ]);
            if ![
                "edit",
                "start",
                "continue",
                "transition",
                "review",
                "cancel",
                "defer",
            ]
            .iter()
            .any(|a| eq(request, "action", a))
            {
                vec![task]
            } else {
                let previous = self
                    .store
                    .read(
                        "task-board/task",
                        &Json::object([
                            ("objectId", request.required("taskId")?.clone()),
                            ("revision", request.required("expectedRevision")?.clone()),
                        ]),
                        true,
                    )
                    .await?;
                let shared = if eq(request, "action", "edit") {
                    !same(
                        f(f(&previous.value, "fields"), "category"),
                        f(f(request, "fields"), "category"),
                    )? || request
                        .get("parentTaskId")
                        .is_some_and(|parent| parent != f(&previous.value, "parentTaskId"))
                        || !same(
                            f(f(&previous.value, "fields"), "dependencies"),
                            f(f(request, "fields"), "dependencies"),
                        )?
                } else if eq(request, "action", "start") || eq(request, "action", "continue") {
                    !f(f(&previous.value, "fields"), "dependencies")
                        .as_array()
                        .ok_or_else(IvyError::internal)?
                        .is_empty()
                        || truthy(f(&previous.value, "blocker"))
                } else if eq(request, "action", "defer") {
                    truthy(f(request, "blocker"))
                } else {
                    true
                };
                if shared {
                    vec!["TaskBoard publication gate".into(), task]
                } else {
                    vec![task]
                }
            }
        };
        self.store.retain_operation_gates(id, &names)?;
        Ok(names)
    }
    pub async fn current(&self, name: &JsString) -> IvyResult<Document> {
        let mut value = self.store.named_gate(name).await?;
        if value.is_none() {
            let initial = Json::object([
                ("schemaVersion", Json::Number(1.0)),
                ("principalId", Json::String(self.principal_id.clone())),
                ("activeOperation", Json::Null),
                ("updatedAt", (self.now)().into()),
            ]);
            let create = Json::object([(
                "create",
                Json::object([
                    (
                        "parentId",
                        self.store
                            .root_object_id()
                            .map(Json::String)
                            .unwrap_or(Json::Null),
                    ),
                    ("name", Json::String(name.clone())),
                ]),
            )]);
            let created = async {
                let pin = self
                    .store
                    .write(
                        "task-board/coordination",
                        &initial,
                        &mutation(
                            &self.store.root_object_id().unwrap_or_else(|| "root".into()),
                            concat(["coordination:".into(), name.clone()]),
                        )?,
                        &create,
                    )
                    .await?;
                self.store.read("task-board/coordination", &pin, true).await
            }
            .await;
            match created {
                Ok(document) => value = Some(document),
                Err(e) if matches!(e.code.as_str(), "mutation_conflict" | "revision_conflict") => {
                    value = self.store.named_gate(name).await?
                }
                Err(e) => return Err(e),
            }
        }
        require(
            value.as_ref().is_some_and(|v| {
                f(&v.value, "principalId").as_js_string() == Some(self.principal_id)
            }),
            "task_board_principal_mismatch",
            "This workflow publication gate belongs to another logical principal.",
        )?;
        Ok(value.unwrap())
    }
    pub async fn acquire(&self, operation: &Document) -> IvyResult<()> {
        let id = s(&operation.pin, "objectId")?;
        let pin = Json::object([
            ("objectId", Json::String(id.clone())),
            ("revision", Json::Number(1.0)),
        ]);
        for name in self.gates(operation).await? {
            let current = self.current(&name).await?;
            if truthy(f(&current.value, "activeOperation")) {
                require(
                    same(f(&current.value, "activeOperation"), &pin)?,
                    "task_board_publication_busy",
                    "Reconcile the recorded publication before starting another workflow action.",
                )?;
                continue;
            }
            let mut next = current.value.clone();
            next.insert("activeOperation", pin.clone());
            next.insert("updatedAt", operation.value.required("createdAt")?.clone());
            self.store
                .write(
                    "task-board/coordination",
                    &next,
                    &mutation(
                        s(&current.pin, "objectId")?,
                        concat([
                            "acquire:".into(),
                            js_string(current.pin.required("revision")?),
                            ":".into(),
                            id.clone(),
                        ]),
                    )?,
                    &Json::object([
                        ("objectId", current.pin.required("objectId")?.clone()),
                        (
                            "expectedRevision",
                            current.pin.required("revision")?.clone(),
                        ),
                    ]),
                )
                .await?;
        }
        Ok(())
    }
    pub async fn reserved(&self, operation: &Document) -> IvyResult<bool> {
        let first = f(&operation.value, "writes")
            .as_array()
            .and_then(|v| v.first());
        let Some(first) = first else { return Ok(false) };
        if !eq(first, "contractKey", "task-board/task") || !truthy(f(first, "outcome")) {
            return Ok(false);
        }
        let payload = self
            .store
            .read("task-board/task", f(f(first, "payload"), "object"), false)
            .await?;
        require(
            f(f(&payload.value, "publication"), "objectId") == f(&operation.pin, "objectId")
                && f(f(&payload.value, "publication"), "revision") == &Json::Number(1.0),
            "task_board_publication_pending",
            "The first Task write must durably retain its owning publication marker.",
        )?;
        require(
            f(first, "destination").get("create").is_some()
                || f(f(first, "destination"), "objectId") == f(f(first, "outcome"), "objectId"),
            "task_board_stage_conflict",
            "The retained Task reservation receipt names another destination.",
        )?;
        Ok(true)
    }
    pub async fn release(&self, id: &JsString, reserved: bool) -> IvyResult<()> {
        let operation = self
            .store
            .read("task-board/operation", &Json::String(id.clone()), true)
            .await?;
        let task_reserved = reserved && self.reserved(&operation).await?;
        for name in self.gates(&operation).await? {
            let current = self.current(&name).await?;
            if f(f(&current.value, "activeOperation"), "objectId").as_js_string() != Some(id) {
                continue;
            }
            require(
                finished(&operation.value) || task_reserved,
                "task_board_publication_pending",
                "An unfinished publication gate cannot be released before its first Task reservation is durable.",
            )?;
            self.clear(&current, id, operation.value.required("updatedAt")?)
                .await?;
        }
        Ok(())
    }
    pub async fn release_unstarted(&self, id: &JsString) -> IvyResult<bool> {
        let operation = self
            .store
            .read("task-board/operation", &Json::String(id.clone()), true)
            .await?;
        if finished(&operation.value)
            || !f(&operation.value, "writes")
                .as_array()
                .ok_or_else(IvyError::internal)?
                .is_empty()
        {
            return Ok(false);
        }
        let mut released = false;
        for name in self.gates(&operation).await? {
            let current = self.current(&name).await?;
            if f(f(&current.value, "activeOperation"), "objectId").as_js_string() != Some(id) {
                continue;
            }
            self.clear(&current, id, operation.value.required("updatedAt")?)
                .await?;
            released = true
        }
        Ok(released)
    }
    async fn clear(&self, current: &Document, id: &JsString, at: &Json) -> IvyResult<()> {
        let mut next = current.value.clone();
        next.insert("activeOperation", Json::Null);
        next.insert("updatedAt", at.clone());
        self.store
            .write(
                "task-board/coordination",
                &next,
                &mutation(
                    s(&current.pin, "objectId")?,
                    concat([
                        "release:".into(),
                        js_string(current.pin.required("revision")?),
                        ":".into(),
                        id.clone(),
                    ]),
                )?,
                &Json::object([
                    ("objectId", current.pin.required("objectId")?.clone()),
                    (
                        "expectedRevision",
                        current.pin.required("revision")?.clone(),
                    ),
                ]),
            )
            .await?;
        Ok(())
    }
    pub fn pending(&self) -> IvyResult<Option<Json>> {
        self.store.pending_coordination()
    }
}
