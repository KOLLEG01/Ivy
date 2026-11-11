//! Public upload ownership, alongside the existing isolated allocation probe.
use super::*;
use ivy_contracts::{Outcome, checked_native_contract::CheckedNativeContracts, hash::hash_json};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_task_board::{
    engine_admission::{AdmissionExecution, AdmissionRuntime, EngineAdmission, EngineOwner},
    engine_read::ReadContext,
    journal_store::Contracts,
};
use std::{path::Path, sync::Arc};

struct UploadStore {
    document: RefCell<Option<Document>>,
    original_pointer: usize,
    baseline: Cell<isize>,
    body_bytes: isize,
    writes: Cell<usize>,
    read_live: Cell<isize>,
}
impl Store for UploadStore {
    fn root_object_id(&self) -> Option<JsString> {
        Some("root".into())
    }
    fn contract_version(&self, _: &str) -> IvyResult<String> {
        unreachable!()
    }
    fn retired_operation(&self, caller: &JsString, id: &JsString) -> IvyResult<bool> {
        assert_eq!(caller, &JsString::from("user"));
        assert_eq!(id, &JsString::from("upload"));
        Ok(false)
    }
    fn local_key(&self, _: &JsString) -> IvyResult<Option<String>> {
        unreachable!()
    }
    fn complete_local_workflow(&self, _: &JsString, _: &JsString) -> IvyResult<()> {
        unreachable!()
    }
    async fn named(
        &self,
        key: &str,
        _: &str,
        parent: Option<&JsString>,
    ) -> IvyResult<Option<Document>> {
        assert_eq!(key, "task-board/operation");
        assert!(parent.is_none());
        Ok(None)
    }
    async fn read(&self, key: &str, address: &Json, primary: bool) -> IvyResult<Document> {
        assert_eq!(key, "task-board/operation");
        assert!(primary);
        let live = LIVE.load(Ordering::Relaxed);
        self.read_live.set(live);
        assert!(
            live <= self.baseline.get() - self.body_bytes + 128 * 1024,
            "Acceptance input still retained while reading its durable record: {live}"
        );
        let document = self.document.borrow_mut().take().unwrap();
        assert_eq!(address, &document.pin.to_json());
        Ok(document)
    }
    async fn write(
        &self,
        key: &str,
        value: &Json,
        mutation: &str,
        destination: &Json,
    ) -> IvyResult<Json> {
        assert_eq!(key, "task-board/operation");
        assert!(!mutation.is_empty());
        assert_eq!(f(f(destination, "create"), "parentId"), &Json::from("root"));
        let pointer = f(f(value, "request"), "bytesBase64")
            .as_js_string()
            .unwrap()
            .0
            .as_ptr() as usize;
        assert_eq!(
            pointer, self.original_pointer,
            "Acceptance must move the admitted original body"
        );
        let saved = self.document.borrow();
        let saved = saved.as_ref().unwrap();
        assert_eq!(
            hash_json(value)?,
            saved.content_hash,
            "All original acceptance bytes and binding remain exact"
        );
        self.writes.set(self.writes.get() + 1);
        Ok(saved.pin.to_json())
    }
    async fn prepared_mutation_id(&self, _: &str) -> IvyResult<JsString> {
        unreachable!()
    }
    async fn write_prepared(
        &self,
        _: &str,
        _: &Json,
        _: &JsString,
        _: &Json,
        _: Option<&JsString>,
        _: Option<&Json>,
    ) -> IvyResult<Json> {
        unreachable!()
    }
}
struct Current;
impl Contracts for Current {
    fn version(&self, _: &str) -> IvyResult<String> {
        unreachable!()
    }
    fn local(&self, _: &str) -> IvyResult<bool> {
        unreachable!()
    }
    fn readable_version(&self, _: &str, _: &JsString) -> IvyResult<bool> {
        unreachable!()
    }
    fn validate_record(&self, _: &str, _: &JsString, _: &Json) -> IvyResult<()> {
        unreachable!()
    }
    fn validate_definition(&self, definition: &str, _: &Json) -> IvyResult<()> {
        assert_eq!(definition, "Settings");
        Ok(())
    }
}
struct Owner;
impl RpcClient for Owner {
    fn request<'a>(&'a self, method: &'a str, args: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            assert_eq!(method, "serviceNodes.get");
            assert_eq!(args, Json::object([("serviceNodeId", "node".into())]));
            Ok(Json::object([
                ("principalId", "owner".into()),
                ("serviceName", "task-board".into()),
                ("connected", true.into()),
                ("synced", true.into()),
            ]))
        })
    }
}
struct Execution<'a> {
    store: &'a UploadStore,
    expected_pointer: usize,
    called: Cell<bool>,
}
impl AdmissionExecution for Execution<'_> {
    async fn execute(&self, document: Document) -> IvyResult<Json> {
        assert!(
            LIVE.load(Ordering::Relaxed)
                <= self.store.baseline.get() - self.store.body_bytes + 128 * 1024,
            "The public request must be released before retained execution starts"
        );
        assert_eq!(
            f(f(&document.value, "request"), "bytesBase64")
                .as_js_string()
                .unwrap()
                .0
                .as_ptr() as usize,
            self.expected_pointer
        );
        self.called.set(true);
        Err(
            IvyError::new("publication_observed", "Stop after actual admission")
                .with_outcome(Outcome::Unknown),
        )
    }
}
#[test]
fn owned_upload_admission_moves_eight_mib_and_releases_input_before_execution() {
    if isolated(
        "upload_admission::owned_upload_admission_moves_eight_mib_and_releases_input_before_execution",
    ) {
        return;
    }
    let request = Json::object([
        ("action", "uploadAttachment".into()),
        ("operationId", "upload".into()),
        ("taskId", "task".into()),
        ("expectedRevision", 1.0.into()),
        ("attachmentId", "file".into()),
        ("filename", "file.bin".into()),
        ("mediaType", "application/octet-stream".into()),
        ("bytesBase64", ("A".repeat(11_184_811) + "=").into()),
    ]);
    let body = f(&request, "bytesBase64").as_js_string().unwrap();
    let original_pointer = body.0.as_ptr() as usize;
    let body_bytes = (body.0.capacity() * 2) as isize;
    let cloned_body_bytes = (body.0.len() * 2) as isize;
    let actor = Json::object([
        ("principalId", "user".into()),
        ("serviceNodeId", "node".into()),
        ("generation", 7.0.into()),
        ("source", "user".into()),
    ]);
    // The separately owned original read is prepared before measuring. The
    // real SQLite test covers persistence/restart; this source detects extra
    // admission copies and the lifetime of the owned write input.
    let value = Json::object([
        ("schemaVersion", 1.0.into()),
        ("operationId", "upload".into()),
        ("callerPrincipalId", "user".into()),
        ("request", request.clone()),
        ("requestHash", hash_json(&request).unwrap().into()),
        ("actor", actor),
        ("createdAt", "now".into()),
        ("updatedAt", "now".into()),
        ("phase", "accepted".into()),
        ("writes", Json::Array(vec![])),
        ("outcome", Json::Null),
        ("error", Json::Null),
    ]);
    let expected_pointer = f(f(&value, "request"), "bytesBase64")
        .as_js_string()
        .unwrap()
        .0
        .as_ptr() as usize;
    let operations = Operations {
        store: UploadStore {
            document: RefCell::new(Some(Document {
                pin: ObjectPin {
                    object_id: "operation".into(),
                    revision: 1.0,
                },
                content_hash: hash_json(&value).unwrap(),
                value,
                parent_id: Some("root".into()),
            })),
            original_pointer,
            body_bytes,
            baseline: Cell::new(0),
            writes: Cell::new(0),
            read_live: Cell::new(0),
        },
        now: || "now".to_owned(),
    };
    let execution = Execution {
        store: &operations.store,
        expected_pointer,
        called: Cell::new(false),
    };
    let settings = Json::object([
        ("principalId", "owner".into()),
        ("rootObjectId", "root".into()),
    ]);
    let owner = EngineOwner {
        service_node_id: "node".into(),
        generation: 7.0,
    };
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let runtime = AdmissionRuntime {
        client: Arc::new(Owner),
        contracts: &Current,
        catalogs: &catalogs,
        handoff: &|_, _| Box::pin(async { panic!("Uploads do not bind native handoffs") }),
    };
    let engine =
        EngineAdmission::new(&operations, &settings, &owner, &runtime, &execution).unwrap();
    let caller = JsString::from("user");
    let id = JsString::from("upload");
    let context = ReadContext {
        caller_principal_id: &caller,
        operation_id: &id,
        generation: 7.0,
        check_cancelled: &|| Ok(()),
    };
    let former_clone_peak = peak(|| drop(request.clone()));
    assert!(former_clone_peak >= cloned_body_bytes);
    let baseline = LIVE.load(Ordering::Relaxed);
    operations.store.baseline.set(baseline);
    let added = peak(|| {
        let error = ready(engine.invoke(request, &context)).unwrap_err();
        assert_eq!(error.code, "publication_observed");
        assert_eq!(error.outcome, Outcome::Unknown);
    });
    assert!(execution.called.get());
    assert_eq!(operations.store.writes.get(), 1);
    assert!(
        added < 128 * 1024,
        "Admission allocated a full upload body: {added}"
    );
    eprintln!(
        "Owned 8 MiB upload admission: former input clone={former_clone_peak} B; additional peak={added} B; original UTF-16 body={body_bytes} B; released before durable read/execution={} B",
        baseline - operations.store.read_live.get()
    );
}
