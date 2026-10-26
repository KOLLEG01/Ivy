#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_task_board::operations::{Document, Operations, Store};
use std::{
    cell::RefCell,
    collections::HashMap,
    future::Future,
    path::Path,
    process::Command,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

#[path = "../fixtures/task-board-comment-reads.rs"]
mod comment_reads;
#[path = "../fixtures/task-board-condition.rs"]
mod condition_policy;
#[path = "../fixtures/task-board-configuration.rs"]
mod configuration;
#[path = "../fixtures/task-board-conversation.rs"]
mod conversation_policy;
#[path = "../fixtures/task-board-current-history.rs"]
mod current_history;
#[path = "../fixtures/task-board-deliveries.rs"]
mod deliveries;
#[path = "../fixtures/task-board-durable-admission.rs"]
mod durable_admission;
#[path = "../fixtures/task-board-lifecycle.rs"]
mod lifecycle;
#[path = "../fixtures/task-board-persistent-reads.rs"]
mod persistent_reads;
#[path = "../fixtures/task-board-program.rs"]
mod publication;
#[path = "../fixtures/task-board-record-upgrades.rs"]
mod record_upgrades;
#[path = "../fixtures/task-board-recovery.rs"]
mod recovery;
#[path = "../fixtures/task-board-scheduler.rs"]
mod scheduler;
#[path = "../fixtures/task-board-keys.rs"]
mod task_keys;
#[path = "../fixtures/task-board-tree-comments.rs"]
mod tree_comments;

fn s(value: &Json, key: &str) -> JsString {
    value.required(key).unwrap().as_js_string().unwrap().clone()
}
fn text(value: &str) -> Json {
    value.into()
}
fn pin(id: &str, revision: f64) -> Json {
    Json::object([
        ("objectId", id.into()),
        ("revision", Json::Number(revision)),
    ])
}
fn document_result(document: Document) -> Json {
    Json::object([("pin", document.pin), ("value", document.value)])
}
#[derive(Clone)]
struct Stored {
    key: String,
    name: String,
    document: Document,
}
struct Memory {
    documents: RefCell<Vec<Stored>>,
    trace: RefCell<Vec<Json>>,
    faults: RefCell<Json>,
    receipts: RefCell<HashMap<JsString, Json>>,
    effects: RefCell<usize>,
}
impl Default for Memory {
    fn default() -> Self {
        Self {
            documents: RefCell::default(),
            trace: RefCell::default(),
            faults: RefCell::new(Json::Object(vec![])),
            receipts: RefCell::default(),
            effects: RefCell::default(),
        }
    }
}
impl Memory {
    fn record(&self, row: impl IntoIterator<Item = Json>) {
        self.trace
            .borrow_mut()
            .push(Json::Array(row.into_iter().collect()));
    }
    fn flag(&self, key: &str) -> bool {
        self.faults
            .borrow()
            .get(key)
            .and_then(Json::as_bool)
            .unwrap_or(false)
    }
    fn set(&self, key: &str, value: Json) {
        self.faults.borrow_mut().insert(key, value);
    }
}
impl Store for Memory {
    fn root_object_id(&self) -> Option<JsString> {
        Some("root".into())
    }
    fn contract_version(&self, _key: &str) -> IvyResult<String> {
        Ok("1.0.0".into())
    }
    fn retired_operation(&self, caller: &JsString, id: &JsString) -> IvyResult<bool> {
        self.record([
            text("retired"),
            Json::String(caller.clone()),
            Json::String(id.clone()),
        ]);
        Ok(self.flag("retired"))
    }
    fn local_key(&self, id: &JsString) -> IvyResult<Option<String>> {
        self.record([text("localKey"), Json::String(id.clone())]);
        self.faults
            .borrow()
            .get("localKey")
            .map(Json::as_str)
            .transpose()
    }
    fn complete_local_workflow(&self, id: &JsString, at: &JsString) -> IvyResult<()> {
        self.record([
            text("complete"),
            Json::String(id.clone()),
            Json::String(at.clone()),
        ]);
        Ok(())
    }
    async fn named(
        &self,
        key: &str,
        name: &str,
        parent: Option<&JsString>,
    ) -> IvyResult<Option<Document>> {
        self.record([
            text("named"),
            text(key),
            text(name),
            parent.cloned().map(Json::String).unwrap_or(Json::Null),
        ]);
        let parent = parent.cloned().unwrap_or_else(|| "root".into());
        Ok(self
            .documents
            .borrow()
            .iter()
            .find(|d| {
                d.key == key && d.name == name && d.document.parent_id.as_ref() == Some(&parent)
            })
            .map(|d| d.document.clone()))
    }
    async fn read(&self, key: &str, address: &Json, upgrade: bool) -> IvyResult<Document> {
        self.record([
            text("read"),
            text(key),
            address.clone(),
            Json::Bool(upgrade),
        ]);
        if address.as_js_string().is_none() && self.flag("readConflict") {
            self.set("readConflict", Json::Bool(false));
            return Err(IvyError::new("revision_conflict", "Fixture read conflict."));
        }
        let id = address
            .as_js_string()
            .cloned()
            .unwrap_or_else(|| s(address, "objectId"));
        self.documents
            .borrow()
            .iter()
            .find(|d| s(&d.document.pin, "objectId") == id)
            .map(|d| d.document.clone())
            .ok_or_else(|| IvyError::new("not_found", "Missing fixture document."))
    }
    async fn write(
        &self,
        key: &str,
        value: &Json,
        mutation: &str,
        destination: &Json,
    ) -> IvyResult<Json> {
        self.record([
            text("write"),
            text(key),
            value.clone(),
            text(mutation),
            destination.clone(),
        ]);
        let conflicts = self
            .faults
            .borrow()
            .get("conflicts")
            .and_then(Json::as_f64)
            .unwrap_or(0.0);
        if destination.get("create").is_none() && conflicts > 0.0 {
            self.set("conflicts", Json::Number(conflicts - 1.0));
            return Err(IvyError::new("revision_conflict", "Fixture CAS conflict."));
        }
        let mut docs = self.documents.borrow_mut();
        let existing = destination
            .get("objectId")
            .and_then(Json::as_js_string)
            .and_then(|id| {
                docs.iter()
                    .position(|d| s(&d.document.pin, "objectId") == *id)
            });
        let (name, parent, pin) = if let Some(index) = existing {
            let old = &docs[index];
            let id = s(&old.document.pin, "objectId").to_utf8().unwrap();
            (
                old.name.clone(),
                old.document.parent_id.clone(),
                pin(
                    &id,
                    old.document.pin.required("revision")?.as_f64().unwrap() + 1.0,
                ),
            )
        } else {
            let create = destination.required("create")?;
            (
                create.required("name")?.as_str()?,
                create.required("parentId")?.as_js_string().cloned(),
                pin(&format!("doc{}", docs.len() + 1), 1.0),
            )
        };
        let stored = Stored {
            key: key.into(),
            name,
            document: Document {
                pin: pin.clone(),
                value: value.clone(),
                parent_id: parent,
                content_hash: hash_json(value)?,
            },
        };
        if let Some(index) = existing {
            docs[index] = stored;
        } else {
            docs.push(stored);
        }
        if destination.get("create").is_some() && self.flag("createConflict") {
            self.set("createConflict", Json::Bool(false));
            return Err(IvyError::new(
                "mutation_conflict",
                "Fixture lost create reply.",
            ));
        }
        Ok(pin)
    }
    async fn prepared_mutation_id(&self, mutation: &str) -> IvyResult<JsString> {
        self.record([text("prepared"), text(mutation)]);
        Ok(format!("prepared:{mutation}").into())
    }
    async fn write_prepared(
        &self,
        key: &str,
        value: &Json,
        mutation: &JsString,
        destination: &Json,
    ) -> IvyResult<Json> {
        self.record([
            text("apply"),
            text(key),
            value.clone(),
            Json::String(mutation.clone()),
            destination.clone(),
        ]);
        let mut receipts = self.receipts.borrow_mut();
        if !receipts.contains_key(mutation) {
            *self.effects.borrow_mut() += 1;
            receipts.insert(mutation.clone(), pin("destination", 2.0));
        }
        if self.flag("unknown") {
            self.set("unknown", Json::Bool(false));
            return Err(
                IvyError::new("outcome_unknown", "Fixture lost external reply.")
                    .with_outcome(Outcome::Unknown),
            );
        }
        Ok(receipts[mutation].clone())
    }
}
struct NoWake;
impl Wake for NoWake {
    fn wake(self: Arc<Self>) {}
}
fn ready<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(NoWake));
    let mut cx = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("Fixture unexpectedly pending"),
    }
}

#[test]
fn retained_protocol_matches_original_outcomes_bytes_and_effect_order() {
    let cases = if let Ok(path) = std::env::var("IVY_TASK_BOARD_OPERATIONS_FIXTURE") {
        parse(
            &std::fs::read_to_string(path).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap()
    } else {
        let result = reference_data::read(include_bytes!("../fixtures/task-board-operations-cases.json.gz"));
        parse(
            std::str::from_utf8(&result).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap()
    };
    for case in cases.as_array().unwrap() {
        let engine = Operations {
            store: Memory::default(),
            now: || "2026-01-02T03:04:05.000Z".into(),
        };
        let mut outcomes = vec![];
        for step in case.required("steps").unwrap().as_array().unwrap() {
            if let Some(patch) = step.get("patch") {
                let mut docs = engine.store.documents.borrow_mut();
                let doc = docs
                    .iter_mut()
                    .find(|d| s(&d.document.pin, "objectId") == s(patch, "id"))
                    .unwrap();
                if let Some(parent) = patch.get("parentId") {
                    doc.document.parent_id = parent.as_js_string().cloned();
                }
                if let Some(hash) = patch.get("contentHash") {
                    doc.document.content_hash = hash.as_str().unwrap();
                }
                continue;
            }
            if let Some(faults) = step.get("fault") {
                for (key, value) in faults.as_object().unwrap() {
                    engine
                        .store
                        .faults
                        .borrow_mut()
                        .insert(key.clone(), value.clone());
                }
                continue;
            }
            let args = step.required("args").unwrap().as_array().unwrap();
            let id = || args[0].as_js_string().unwrap().clone();
            let result = ready(async {
                match step.required("method")?.as_str()?.as_str() {
                    "begin" => engine.begin(&args[0], &args[1]).await.map(document_result),
                    "stage" => {
                        engine
                            .stage(
                                &id(),
                                args[1].as_f64().unwrap(),
                                &args[2].as_str()?,
                                &args[3],
                                &args[4],
                            )
                            .await
                    }
                    "apply" => {
                        engine
                            .apply(&id(), args[1].as_f64().unwrap() as usize)
                            .await
                    }
                    "finish" => engine.finish(&id(), &args[1]).await,
                    "archiveIdentity" => engine.archive_identity(&id()).await.map(Json::String),
                    "prepareNative" => engine.prepare_native(&id(), &args[1]).await,
                    "observeNative" => {
                        engine
                            .observe_native(&id(), args[1].as_js_string().unwrap(), &args[2])
                            .await
                    }
                    "failure" => {
                        let error = IvyError::new_js(s(&args[1], "code"), s(&args[1], "message"))
                            .with_provider_outcome(args[1].required("outcome")?.clone());
                        engine
                            .failure(&id(), &error, args[2].as_bool().unwrap())
                            .await
                            .map(document_result)
                    }
                    method => panic!("Unknown fixture method {method}"),
                }
            });
            outcomes.push(match result {
                Ok(value) => value,
                Err(error) => Json::object([(
                    "error",
                    Json::object([
                        ("code", Json::String(error.code_js())),
                        ("message", Json::String(error.message_js())),
                        ("outcome", error.outcome.to_json()),
                    ]),
                )]),
            });
        }
        let actual = Json::object([
            ("outcomes", Json::Array(outcomes)),
            ("trace", Json::Array(engine.store.trace.into_inner())),
            (
                "effects",
                Json::Number(engine.store.effects.into_inner() as f64),
            ),
        ]);
        assert_eq!(
            canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap(),
            canonical(case.required("expected").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
            "{}",
            case.required("name").unwrap().as_str().unwrap()
        );
    }
}
