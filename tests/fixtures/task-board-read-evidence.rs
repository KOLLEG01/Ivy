#[path = "reference-data.rs"]
mod reference_data;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    builtin_schema::SchemaBundle,
    checked_native_contract::CheckedNativeContracts,
    hash::{digest, hash_json},
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_task_board::{
    evidence_bytes::{BlobStore, ChunkKey},
    operations::Document,
    read_evidence::{ReadEvidence, ReadEvidenceStore, ReadIdentity},
};
use std::{
    cell::{Cell, RefCell},
    future::Future,
    path::Path,
    process::Command,
    sync::{Arc, OnceLock},
    task::{Context, Poll, Wake, Waker},
};

fn s(v: &Json, k: &str) -> JsString {
    v.required(k).unwrap().as_js_string().unwrap().clone()
}
fn merge(v: &Json, patch: Option<&Json>) -> Json {
    let mut value = v.clone();
    if let Some(fields) = patch.and_then(Json::as_object) {
        for (k, v) in fields {
            value.insert(k.clone(), v.clone());
        }
    }
    value
}
fn pin(id: &str) -> Json {
    Json::object([("objectId", id.into()), ("revision", Json::Number(1.0))])
}
struct Blob {
    name: String,
    bytes: Vec<u8>,
    pin: Json,
}
struct Memory<'a> {
    row: &'a Json,
    run: &'a Json,
    active: Cell<bool>,
    trace: RefCell<Vec<Json>>,
    document: RefCell<Option<Document>>,
    blobs: RefCell<Vec<Blob>>,
}
impl Memory<'_> {
    fn trace(&self, row: impl IntoIterator<Item = Json>) {
        self.trace
            .borrow_mut()
            .push(Json::Array(row.into_iter().collect()));
    }
    fn flag(&self, key: &str) -> bool {
        self.row.get(key).and_then(Json::as_bool) == Some(true)
    }
    fn fail(&self, key: &str) -> IvyResult<()> {
        if self.active.get()
            && self.row.get("fail").and_then(Json::as_js_string) == Some(&key.into())
        {
            Err(IvyError::new("fixture_failed", key))
        } else {
            Ok(())
        }
    }
}
impl BlobStore for Memory<'_> {
    type Pin = Json;
    type Error = IvyError;
    fn save(&self, key: ChunkKey, name: &str, parent: &JsString, bytes: &[u8]) -> IvyResult<Json> {
        self.trace([
            "blob-save".into(),
            key.as_str().into(),
            name.into(),
            Json::String(parent.clone()),
            digest(bytes).into(),
            Json::Number(bytes.len() as f64),
        ]);
        self.fail(&format!("chunk-{}", self.blobs.borrow().len()))?;
        let mut blobs = self.blobs.borrow_mut();
        if let Some(blob) = blobs.iter().find(|b| b.name == name) {
            return Ok(blob.pin.clone());
        }
        let pin = pin(&format!("00000000-0000-4000-8000-{:012}", blobs.len() + 1));
        blobs.push(Blob {
            name: name.into(),
            bytes: bytes.to_vec(),
            pin: pin.clone(),
        });
        Ok(pin)
    }
    fn read(&self, key: ChunkKey, pin: &Json, parent: &JsString) -> IvyResult<Vec<u8>> {
        self.trace([
            "blob-read".into(),
            key.as_str().into(),
            pin.clone(),
            Json::String(parent.clone()),
        ]);
        self.fail("blob-read")?;
        if self.flag("wrongBlob") {
            return Ok(b"wrong".to_vec());
        }
        Ok(self
            .blobs
            .borrow()
            .iter()
            .find(|b| b.pin == *pin)
            .unwrap()
            .bytes
            .clone())
    }
}
impl ReadEvidenceStore for Memory<'_> {
    fn validate(&self, definition: &str, value: &Json) -> IvyResult<()> {
        // Exercise the real static schema evaluator against the same repository
        // definitions. Production wiring belongs to the shared TaskBoard validator.
        static BUNDLE: OnceLock<SchemaBundle> = OnceLock::new();
        let bundle = BUNDLE.get_or_init(|| {
            SchemaBundle::new(&[
                include_str!("../../specs/schemas/task-board.schema.json"),
                include_str!("../../specs/schemas/chat.schema.json"),
                include_str!("../../specs/schemas/agent.schema.json"),
                include_str!("../../specs/schemas/hive-wire.schema.json"),
            ])
            .unwrap()
        });
        canonical(value, MANAGEMENT_FRAME_BYTES)?;
        if bundle.matches(
            &format!("https://ivy.invalid/schemas/task-board.schema.json#/$defs/{definition}"),
            value,
        )? {
            Ok(())
        } else {
            Err(IvyError::new(
                "invalid_arguments",
                "Value does not match its TaskBoard workflow contract.",
            ))
        }
    }
    async fn read_document(&self, key: &str, address: &Json, upgrade: bool) -> IvyResult<Document> {
        self.trace([
            "read".into(),
            key.into(),
            address.clone(),
            Json::Bool(upgrade),
        ]);
        if key == "task-board/run" {
            self.fail("scope")?;
            Ok(Document {
                pin: address.clone(),
                value: merge(
                    self.run,
                    if self.active.get() {
                        self.row.get("run")
                    } else {
                        None
                    },
                ),
                parent_id: None,
                content_hash: String::new(),
            })
        } else {
            self.fail("manifest-read")?;
            Ok(self.document.borrow().as_ref().unwrap().clone())
        }
    }
    async fn write_document(
        &self,
        key: &str,
        value: &Json,
        id: &str,
        destination: &Json,
    ) -> IvyResult<Json> {
        self.trace([
            "write".into(),
            key.into(),
            value.clone(),
            id.into(),
            destination.clone(),
        ]);
        self.fail("manifest-write")?;
        let pin = pin("00000000-0000-4000-8000-000000000900");
        *self.document.borrow_mut() = Some(Document {
            pin: pin.clone(),
            value: value.clone(),
            parent_id: Some(s(destination.required("create")?, "parentId")),
            content_hash: hash_json(value)?,
        });
        Ok(pin)
    }
}
struct Noop;
impl Wake for Noop {
    fn wake(self: Arc<Self>) {}
}
fn ready<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let waker = Waker::from(Arc::new(Noop));
    match future.as_mut().poll(&mut Context::from_waker(&waker)) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("Fixture unexpectedly yielded"),
    }
}
fn identity(value: &Json) -> ReadIdentity {
    ReadIdentity {
        observation_id: s(value, "observationId"),
        caller_principal_id: s(value, "callerPrincipalId"),
        service_node_id: s(value, "serviceNodeId"),
        native_version: s(value, "nativeVersion"),
        epoch: s(value, "epoch"),
        method: s(value, "method"),
        params: value.required("params").unwrap().clone(),
    }
}

#[test]
fn scoped_native_read_evidence_matches_original_typescript() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = reference_data::read(include_bytes!("task-board-read-evidence-cases.json.gz"));
    let data = parse(
        std::str::from_utf8(&output).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let base = data.required("base").unwrap();
    let catalogs = CheckedNativeContracts::new(root);
    for row in data.required("rows").unwrap().as_array().unwrap() {
        let store = Memory {
            row,
            run: base.required("run").unwrap(),
            active: Cell::new(false),
            trace: RefCell::default(),
            document: RefCell::default(),
            blobs: RefCell::default(),
        };
        let subject = ReadEvidence {
            store: &store,
            catalogs: &catalogs,
        };
        let parent = row
            .get("parent")
            .unwrap_or(base.required("parent").unwrap());
        let expected = merge(base.required("expected").unwrap(), row.get("expected"));
        let original = identity(base.required("expected").unwrap());
        let mut observation = merge(
            base.required("observation").unwrap(),
            row.get("initialObservation"),
        );
        if store.flag("large") {
            observation.insert(
                "reply",
                Json::object([(
                    "result",
                    Json::object([("text", "x".repeat(1100000).into())]),
                )]),
            );
        }
        let result: IvyResult<Json> = ready(async {
            if row.required("mode")?.as_str()? == "read" {
                let mut artifact = subject.save(parent, &observation, &original).await?;
                store.trace.borrow_mut().clear();
                store.active.set(true);
                {
                    let mut document = store.document.borrow_mut();
                    let doc = document.as_mut().unwrap();
                    doc.value = merge(&doc.value, row.get("manifest"));
                    doc.content_hash = hash_json(&doc.value)?;
                    artifact.insert("contentHash", doc.content_hash.clone().into());
                    if store.flag("wrongParent") {
                        doc.parent_id = Some("other".into());
                    }
                }
                artifact = merge(&artifact, row.get("artifact"));
                subject.read(parent, &artifact, &identity(&expected)).await
            } else {
                store.active.set(true);
                observation = merge(&observation, row.get("observation"));
                if let Some(key) = row.get("remove") {
                    observation.remove(&key.as_str()?);
                }
                if store.flag("wide") {
                    observation.insert(
                        "reply",
                        Json::object([("result", Json::Array(vec![Json::Number(0.0); 400001]))]),
                    );
                }
                let first = subject
                    .save(parent, &observation, &identity(&expected))
                    .await?;
                if row.required("mode")?.as_str()? == "replay" {
                    subject.save(parent, &observation, &original).await
                } else {
                    Ok(first)
                }
            }
        });
        let result = match result {
            Ok(v) => Json::object([("ok", v)]),
            Err(e) => Json::object([(
                "error",
                Json::object([
                    ("code", Json::String(e.code_js())),
                    ("message", Json::String(e.message_js())),
                ]),
            )]),
        };
        let actual = Json::object([
            ("result", result),
            ("trace", Json::Array(store.trace.borrow().clone())),
            (
                "manifest",
                store
                    .document
                    .borrow()
                    .as_ref()
                    .map(|d| d.value.clone())
                    .unwrap_or(Json::Null),
            ),
        ]);
        assert_eq!(
            hash_json(&actual).unwrap(),
            row.required("expectedHash").unwrap().as_str().unwrap(),
            "{}: {}",
            row.required("name").unwrap().as_str().unwrap(),
            canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap()
        );
    }
}
