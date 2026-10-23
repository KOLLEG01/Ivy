//! Durable root/task publication gates over the original retained journal.
use crate::{
    journal_store::{
        Contracts, JournalStore, mutation_ports::WriteFailure, persistent_reads::json_values::Value,
    },
    lifecycle_journal::LifecycleJournal,
    local_gates::LocalGates,
    operations::{Document, Store, decoded},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, json::canonical, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::RpcClient;
use ivy_store::sqlite::error;
#[path = "local_coordination.rs"]
pub(crate) mod local;
#[path = "local_gate_names.rs"]
pub(crate) mod names;
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
fn concat(parts: impl IntoIterator<Item = JsString>) -> JsString {
    JsString(parts.into_iter().flat_map(|s| s.0).collect())
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
    async fn named_graph(&self, name: Value) -> Result<Option<Value>, WriteFailure<()>> {
        let name = decoded::json_result(Ok(name))?
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)?;
        Ok(self.store.named_gate(&name).await?.map(decoded::record))
    }
    async fn gate_names_graph(&self, operation: Value) -> Result<Vec<Value>, WriteFailure<()>> {
        Ok(self
            .gates(&decoded::document(operation)?)
            .await?
            .into_iter()
            .map(Value::String)
            .collect())
    }
    pub async fn current(&self, name: &JsString) -> IvyResult<Document> {
        let store = decoded::DecodedStore(self.store);
        let principal = Value::String(self.principal_id.clone());
        let calls = local::Calls {
            store: &store,
            principal: &principal,
            named: |name| self.named_graph(name),
            gates: |operation| self.gate_names_graph(operation),
            now: || Value::String((self.now)().into()),
        };
        calls
            .current(&Value::String(name.clone()))
            .await
            .map_err(decoded::failure)
            .and_then(decoded::document)
    }
    pub async fn acquire(&self, operation: &Document) -> IvyResult<()> {
        let store = decoded::DecodedStore(self.store);
        let principal = Value::String(self.principal_id.clone());
        let calls = local::Calls {
            store: &store,
            principal: &principal,
            named: |name| self.named_graph(name),
            gates: |operation| self.gate_names_graph(operation),
            now: || Value::String((self.now)().into()),
        };
        calls
            .acquire(&decoded::record(operation.clone()))
            .await
            .map_err(decoded::failure)
    }
    pub async fn reserved(&self, operation: &Document) -> IvyResult<bool> {
        let store = decoded::DecodedStore(self.store);
        let principal = Value::String(self.principal_id.clone());
        let calls = local::Calls {
            store: &store,
            principal: &principal,
            named: |name| self.named_graph(name),
            gates: |operation| self.gate_names_graph(operation),
            now: || Value::String((self.now)().into()),
        };
        calls
            .reserved(&decoded::record(operation.clone()))
            .await
            .map_err(decoded::failure)
    }
    pub async fn release(&self, id: &JsString, reserved: bool) -> IvyResult<()> {
        let store = decoded::DecodedStore(self.store);
        let principal = Value::String(self.principal_id.clone());
        let calls = local::Calls {
            store: &store,
            principal: &principal,
            named: |name| self.named_graph(name),
            gates: |operation| self.gate_names_graph(operation),
            now: || Value::String((self.now)().into()),
        };
        // The public JSON interface keeps the legacy root-release policy.
        calls
            .release(&Value::String(id.clone()), reserved, false)
            .await
            .map_err(decoded::failure)
    }
    pub async fn release_unstarted(&self, id: &JsString) -> IvyResult<bool> {
        let store = decoded::DecodedStore(self.store);
        let principal = Value::String(self.principal_id.clone());
        let calls = local::Calls {
            store: &store,
            principal: &principal,
            named: |name| self.named_graph(name),
            gates: |operation| self.gate_names_graph(operation),
            now: || Value::String((self.now)().into()),
        };
        calls
            .release_unstarted(&Value::String(id.clone()))
            .await
            .map_err(decoded::failure)
    }
    pub fn pending(&self) -> IvyResult<Option<Json>> {
        self.store.pending_coordination()
    }
}
