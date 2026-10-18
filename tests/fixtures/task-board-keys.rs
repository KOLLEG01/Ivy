use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_task_board::{
    operations::{Document, Store, identity_hash},
    task_keys::allocate,
};
use std::{
    cell::{Cell, RefCell},
    path::Path,
    process::Command,
};

const SEQUENCE: &str = "task-board/task-key-sequence";
const ALLOCATION: &str = "task-board/task-key-allocation";

fn string(value: &Json, key: &str) -> JsString {
    value.required(key).unwrap().as_js_string().unwrap().clone()
}
fn sequence(next: f64) -> Json {
    Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("nextValue", Json::Number(next)),
        ("updatedAt", "seed".into()),
    ])
}
fn allocation(id: JsString, key: &Json) -> Json {
    Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("operationId", Json::String(id)),
        ("taskKey", key.clone()),
        ("createdAt", "seed".into()),
    ])
}
fn require(ok: bool, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new("revision_conflict", message))
    }
}
struct Memory {
    root: Option<JsString>,
    documents: RefCell<Vec<Json>>,
    receipts: RefCell<Vec<(String, Json)>>,
    trace: RefCell<Vec<Json>>,
    faults: RefCell<Vec<Json>>,
    skip_first_lookup: Cell<bool>,
}
impl Memory {
    fn insert(&self, key: &str, name: &str, value: &Json) -> Json {
        let mut docs = self.documents.borrow_mut();
        let pin = Json::object([
            ("objectId", format!("doc{}", docs.len() + 1).into()),
            ("revision", Json::Number(1.0)),
        ]);
        docs.push(Json::object([
            ("key", key.into()),
            ("name", name.into()),
            ("pin", pin.clone()),
            (
                "parentId",
                self.root.clone().map(Json::String).unwrap_or(Json::Null),
            ),
            ("value", value.clone()),
            ("contentHash", hash_json(value).unwrap().into()),
        ]));
        pin
    }
    fn record(&self, parts: impl IntoIterator<Item = Json>) {
        self.trace
            .borrow_mut()
            .push(Json::Array(parts.into_iter().collect()));
    }
    fn hit(&self, event: &str, after: bool, value: &Json, destination: &Json) -> IvyResult<()> {
        if event == "named-allocation" && self.skip_first_lookup.replace(false) {
            return Ok(());
        }
        let mut faults = self.faults.borrow_mut();
        let Some(index) = faults.iter().position(|f| {
            f.get("event") == Some(&Json::from(event))
                && f.get("after").and_then(Json::as_bool).unwrap_or(false) == after
        }) else {
            return Ok(());
        };
        let fault = faults.remove(index);
        drop(faults);
        if let Some(winner) = fault.get("winner") {
            let winner_value = if event == "initialize" {
                sequence(winner.required("nextValue")?.as_f64().unwrap())
            } else {
                allocation(
                    winner
                        .get("operationId")
                        .unwrap_or(value.required("operationId")?)
                        .as_js_string()
                        .unwrap()
                        .clone(),
                    winner.required("taskKey")?,
                )
            };
            self.insert(
                if event == "initialize" {
                    SEQUENCE
                } else {
                    ALLOCATION
                },
                &destination.required("create")?.required("name")?.as_str()?,
                &winner_value,
            );
        }
        if let Some(advance) = fault.get("advance").and_then(Json::as_f64) {
            let mut docs = self.documents.borrow_mut();
            let doc = docs
                .iter_mut()
                .find(|d| d.get("key") == Some(&Json::from(SEQUENCE)))
                .unwrap();
            let mut value = doc.required("value")?.clone();
            value.insert(
                "nextValue",
                Json::Number(value.required("nextValue")?.as_f64().unwrap() + advance),
            );
            let mut pin = doc.required("pin")?.clone();
            pin.insert(
                "revision",
                Json::Number(pin.required("revision")?.as_f64().unwrap() + 1.0),
            );
            doc.insert("pin", pin);
            doc.insert("contentHash", hash_json(&value)?.into());
            doc.insert("value", value);
        }
        let outcome = match fault
            .get("outcome")
            .and_then(|v| v.as_str().ok())
            .as_deref()
        {
            Some("unknown") => Outcome::Unknown,
            _ => Outcome::NotExecuted,
        };
        Err(IvyError::new(
            &fault.required("code")?.as_str()?,
            format!("Fixture {event}."),
        )
        .with_outcome(outcome))
    }
}
fn document(value: &Json) -> Document {
    Document {
        pin: value.required("pin").unwrap().clone(),
        value: value.required("value").unwrap().clone(),
        parent_id: value.required("parentId").unwrap().as_js_string().cloned(),
        content_hash: value.required("contentHash").unwrap().as_str().unwrap(),
    }
}
impl Store for Memory {
    fn root_object_id(&self) -> Option<JsString> {
        self.root.clone()
    }
    fn contract_version(&self, _: &str) -> IvyResult<String> {
        unreachable!()
    }
    fn retired_operation(&self, _: &JsString, _: &JsString) -> IvyResult<bool> {
        unreachable!()
    }
    fn local_key(&self, _: &JsString) -> IvyResult<Option<String>> {
        unreachable!()
    }
    fn complete_local_workflow(&self, _: &JsString, _: &JsString) -> IvyResult<()> {
        unreachable!()
    }
    async fn prepared_mutation_id(&self, _: &str) -> IvyResult<JsString> {
        unreachable!()
    }
    async fn write_prepared(&self, _: &str, _: &Json, _: &JsString, _: &Json) -> IvyResult<Json> {
        unreachable!()
    }
    async fn named(
        &self,
        key: &str,
        name: &str,
        parent: Option<&JsString>,
    ) -> IvyResult<Option<Document>> {
        self.record([
            "named".into(),
            key.into(),
            name.into(),
            parent.cloned().map(Json::String).unwrap_or(Json::Null),
        ]);
        self.hit(
            if key == SEQUENCE {
                "named-sequence"
            } else {
                "named-allocation"
            },
            false,
            &Json::Null,
            &Json::Null,
        )?;
        let parent = parent
            .cloned()
            .or_else(|| self.root.clone())
            .map(Json::String)
            .unwrap_or(Json::Null);
        Ok(self
            .documents
            .borrow()
            .iter()
            .find(|d| {
                d.get("key") == Some(&Json::from(key))
                    && d.get("name") == Some(&Json::from(name))
                    && d.get("parentId") == Some(&parent)
            })
            .map(document))
    }
    async fn read(&self, key: &str, address: &Json, upgrade: bool) -> IvyResult<Document> {
        self.record([
            "read".into(),
            key.into(),
            address.clone(),
            Json::Bool(upgrade),
        ]);
        self.hit("read", false, &Json::Null, &Json::Null)?;
        self.documents
            .borrow()
            .iter()
            .find(|d| d.get("key") == Some(&Json::from(key)) && d.get("pin") == Some(address))
            .map(document)
            .ok_or_else(|| IvyError::new("not_found", "Fixture document missing."))
    }
    async fn write(
        &self,
        key: &str,
        value: &Json,
        mutation: &str,
        destination: &Json,
    ) -> IvyResult<Json> {
        self.record([
            "write".into(),
            key.into(),
            value.clone(),
            mutation.into(),
            destination.clone(),
        ]);
        let event = if key == ALLOCATION {
            "retain"
        } else if destination.get("create").is_some() {
            "initialize"
        } else {
            "increment"
        };
        self.hit(event, false, value, destination)?;
        let hash = hash_json(&Json::object([
            ("key", key.into()),
            ("value", value.clone()),
            ("destination", destination.clone()),
        ]))?;
        if let Some((_, receipt)) = self.receipts.borrow().iter().find(|(m, _)| m == mutation) {
            if receipt.get("hash") != Some(&Json::from(hash)) {
                return Err(IvyError::new(
                    "mutation_conflict",
                    "Fixture mutation identity conflict.",
                ));
            }
            return Ok(receipt.required("pin")?.clone());
        }
        let pin = if let Some(create) = destination.get("create") {
            require(
                !self.documents.borrow().iter().any(|d| {
                    d.get("key") == Some(&Json::from(key))
                        && d.get("name") == create.get("name")
                        && d.get("parentId") == create.get("parentId")
                }),
                "Fixture name conflict.",
            )?;
            self.insert(key, &create.required("name")?.as_str()?, value)
        } else {
            let mut docs = self.documents.borrow_mut();
            let doc = docs.iter_mut().find(|d| {
                d.get("pin").and_then(|p| p.get("objectId")) == destination.get("objectId")
            });
            require(
                doc.as_ref().is_some_and(|d| {
                    d.get("pin").and_then(|p| p.get("revision"))
                        == destination.get("expectedRevision")
                }),
                "Fixture CAS conflict.",
            )?;
            let doc = doc.unwrap();
            let mut pin = doc.required("pin")?.clone();
            pin.insert(
                "revision",
                Json::Number(pin.required("revision")?.as_f64().unwrap() + 1.0),
            );
            doc.insert("pin", pin.clone());
            doc.insert("value", value.clone());
            doc.insert("contentHash", hash_json(value)?.into());
            pin
        };
        self.receipts.borrow_mut().push((
            mutation.into(),
            Json::object([("hash", hash.into()), ("pin", pin.clone())]),
        ));
        self.hit(event, true, value, destination)?;
        Ok(pin)
    }
}

#[test]
fn task_key_recovery_matches_original_with_races_and_lost_replies() {
    let result = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-key-oracle.mjs",
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let cases = parse(
        std::str::from_utf8(&result.stdout).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    for case in cases.as_array().unwrap() {
        let store = Memory {
            root: case.required("root").unwrap().as_js_string().cloned(),
            documents: RefCell::default(),
            receipts: RefCell::default(),
            trace: RefCell::default(),
            faults: RefCell::default(),
            skip_first_lookup: Cell::new(false),
        };
        let ticks = Cell::new(0);
        let now = || {
            let n = ticks.get();
            ticks.set(n + 1);
            format!("2026-01-02T03:04:05.{n:03}Z")
        };
        let mut outcomes = vec![];
        for step in case.required("steps").unwrap().as_array().unwrap() {
            if let Some(next) = step.get("sequence") {
                store.insert(
                    SEQUENCE,
                    "Task key sequence",
                    &sequence(next.as_f64().unwrap()),
                );
                continue;
            }
            if let Some(a) = step.get("allocation") {
                let id = string(a, "id");
                let name = format!(
                    "Task key allocation {}",
                    identity_hash(&"task-board".into(), &id).unwrap()
                );
                store.insert(
                    ALLOCATION,
                    &name,
                    &allocation(
                        a.get("owner")
                            .and_then(Json::as_js_string)
                            .cloned()
                            .unwrap_or(id),
                        a.required("taskKey").unwrap(),
                    ),
                );
                continue;
            }
            if let Some(fault) = step.get("fault") {
                if fault
                    .get("armAfterFirst")
                    .and_then(Json::as_bool)
                    .unwrap_or(false)
                {
                    store.skip_first_lookup.set(true);
                }
                store.faults.borrow_mut().push(fault.clone());
                continue;
            }
            let result = super::ready(allocate(&store, &now, &string(step, "allocate")));
            outcomes.push(match result {
                Ok(key) => Json::String(key),
                Err(error) => Json::object([(
                    "error",
                    Json::object([
                        ("code", Json::String(error.code_js())),
                        ("message", Json::String(error.message_js())),
                        ("outcome", error.outcome.as_str().into()),
                    ]),
                )]),
            });
        }
        let actual = Json::object([
            ("outcomes", Json::Array(outcomes)),
            ("trace", Json::Array(store.trace.into_inner())),
            ("documents", Json::Array(store.documents.into_inner())),
            (
                "receipts",
                Json::Array(
                    store
                        .receipts
                        .into_inner()
                        .into_iter()
                        .map(|(key, value)| Json::Array(vec![key.into(), value]))
                        .collect(),
                ),
            ),
            ("ticks", Json::Number(f64::from(ticks.get()))),
        ]);
        assert_eq!(
            canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap(),
            canonical(case.required("expected").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
            "{}",
            case.required("name").unwrap().as_str().unwrap()
        );
    }
}
