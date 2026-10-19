use ivy_contracts::checked_native_contract::CheckedNativeContracts;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, hash::hash_json, json::parse,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_task_board::{
    engine_publication::{
        CreationChecks, EnginePublisher, PublicationRoutes, RootPublicationChecks,
        RootPublicationStore,
    },
    native_lifecycle::LifecyclePublication,
    native_lifecycle::{LifecycleDocument, LifecycleStore},
    operations::{Document, Operations, Store},
    task_keys,
};
use std::{
    collections::VecDeque,
    path::Path,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
};
#[path = "task-board-engine-publication-journal.rs"]
mod journal;
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> &'a JsString {
    v.required(k).unwrap().as_js_string().unwrap()
}
fn data() -> &'static Json {
    static D: OnceLock<Json> = OnceLock::new();
    D.get_or_init(|| {
        let p = Command::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/task-board-engine-publication-oracle.mjs",
            ])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(p.status.success(), "{}", String::from_utf8_lossy(&p.stderr));
        parse(
            std::str::from_utf8(&p.stdout).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap()
    })
}
fn error(v: &Json) -> IvyError {
    IvyError::new_js(s(v, "code").clone(), s(v, "message").clone())
        .with_outcome(match f(v, "outcome").as_str().unwrap().as_str() {
            "unknown" => Outcome::Unknown,
            "completed" => Outcome::Completed,
            _ => Outcome::NotExecuted,
        })
        .with_details(f(v, "details").clone())
}
fn outcome(v: IvyResult<Json>) -> Json {
    match v {
        Ok(v) => Json::object([("ok", v)]),
        Err(e) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(e.code_js())),
                ("message", Json::String(e.message_js())),
                ("outcome", e.outcome.as_str().into()),
                ("details", e.details.unwrap_or(Json::Null)),
            ]),
        )]),
    }
}
struct Replay {
    steps: Mutex<VecDeque<Json>>,
    name: JsString,
}
impl Replay {
    fn call(&self, kind: &str, args: impl IntoIterator<Item = Json>) -> IvyResult<Json> {
        let args = Json::Array(args.into_iter().collect());
        let e = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("{:?}: unexpected {kind}: {args:?}", self.name));
        assert_eq!(
            f(&e, "kind"),
            &Json::from(kind),
            "{:?}: {args:?}",
            self.name
        );
        assert_eq!(
            hash_json(f(&e, "args")).unwrap(),
            hash_json(&args).unwrap(),
            "{:?} {kind}: expected {:?}, actual {args:?}",
            self.name,
            f(&e, "args")
        );
        if let Some(e) = e.get("error") {
            Err(error(e))
        } else {
            Ok(f(&e, "value").clone())
        }
    }
}
struct World {
    replay: Arc<Replay>,
    root: Option<JsString>,
}
fn document(v: &Json) -> Document {
    Document {
        pin: v.required("pin").unwrap().clone(),
        value: v.required("value").unwrap().clone(),
        parent_id: f(f(v, "metadata"), "parentId").as_js_string().cloned(),
        content_hash: hash_json(v.required("value").unwrap()).unwrap(),
    }
}
impl World {
    fn read_record(&self, k: &str, a: &Json, p: bool, archived: bool) -> IvyResult<Json> {
        self.replay.call(
            "read",
            [k.into(), a.clone(), Json::Bool(p), Json::Bool(archived)],
        )
    }
}
impl Store for World {
    fn root_object_id(&self) -> Option<JsString> {
        self.root.clone()
    }
    fn contract_version(&self, key: &str) -> IvyResult<String> {
        Ok(match key {
            "task-board/task" => "1.8.0",
            "task-board/configuration" => "1.2.0",
            "task-board/run" | "task-board/delivery" => "1.1.0",
            _ => "1.0.0",
        }
        .into())
    }
    fn retired_operation(&self, c: &JsString, id: &JsString) -> IvyResult<bool> {
        Ok(self
            .replay
            .call(
                "retired",
                [Json::String(c.clone()), Json::String(id.clone())],
            )?
            .as_bool()
            .unwrap())
    }
    fn local_key(&self, _: &JsString) -> IvyResult<Option<String>> {
        unreachable!()
    }
    fn complete_local_workflow(&self, _: &JsString, _: &JsString) -> IvyResult<()> {
        unreachable!()
    }
    async fn named(&self, k: &str, n: &str, p: Option<&JsString>) -> IvyResult<Option<Document>> {
        let v = self.replay.call(
            "named",
            [
                k.into(),
                n.into(),
                p.cloned().map(Json::String).unwrap_or(Json::Null),
            ],
        )?;
        Ok((!v.is_null()).then(|| document(&v)))
    }
    async fn read(&self, k: &str, a: &Json, p: bool) -> IvyResult<Document> {
        Ok(document(&self.read_record(k, a, p, false)?))
    }
    async fn write(&self, k: &str, v: &Json, m: &str, d: &Json) -> IvyResult<Json> {
        self.replay
            .call("write", [k.into(), v.clone(), m.into(), d.clone()])
    }
    async fn prepared_mutation_id(&self, m: &str) -> IvyResult<JsString> {
        Ok(self
            .replay
            .call("prepared", [m.into()])?
            .as_js_string()
            .unwrap()
            .clone())
    }
    async fn write_prepared(&self, k: &str, v: &Json, m: &JsString, d: &Json) -> IvyResult<Json> {
        self.replay.call(
            "apply",
            [k.into(), v.clone(), Json::String(m.clone()), d.clone()],
        )
    }
}
impl LifecycleStore for World {
    async fn lifecycle_read(
        &self,
        k: &str,
        a: &Json,
        p: bool,
        archived: bool,
    ) -> IvyResult<LifecycleDocument> {
        let v = self.read_record(k, a, p, archived)?;
        Ok(LifecycleDocument {
            document: document(&v),
            metadata: v.required("metadata")?.clone(),
        })
    }
    async fn page(&self, k: &str, o: &Json) -> IvyResult<Json> {
        self.replay.call("page", [k.into(), o.clone()])
    }
    async fn archive(&self, _: &Json) -> IvyResult<Json> {
        unreachable!()
    }
    async fn stat(&self, _: &JsString) -> IvyResult<Json> {
        unreachable!()
    }
    fn release_workspace(&self, _: &JsString) -> IvyResult<()> {
        unreachable!()
    }
}
fn clock() -> String {
    "2026-01-02T03:04:05.006Z".into()
}
impl RootPublicationStore for World {
    async fn allocate_task_key(&self, operation: &JsString) -> IvyResult<JsString> {
        task_keys::allocate(self, &clock, operation).await
    }
}
struct Bindings(Arc<Replay>);
impl CreationChecks for Bindings {
    async fn fields(&self, fields: &Json) -> IvyResult<()> {
        self.0
            .call("fields", [fields.clone(), Json::Null])
            .map(|_| ())
    }
    async fn initial_state(
        &self,
        request: &Json,
        fields: &Json,
        actor: &Json,
        at: &JsString,
    ) -> IvyResult<Json> {
        self.0.call(
            "initialState",
            [
                request.clone(),
                fields.clone(),
                actor.clone(),
                Json::String(at.clone()),
            ],
        )
    }
    async fn execution_options(&self, fields: &Json, allow_unavailable: bool) -> IvyResult<()> {
        self.0
            .call(
                "executionOptions",
                [fields.clone(), Json::Null, allow_unavailable.into()],
            )
            .map(|_| ())
    }
}
impl RootPublicationChecks for Bindings {
    async fn configuration(&self, value: &Json) -> IvyResult<()> {
        self.0.call("configuration", [value.clone()]).map(|_| ())
    }
}
async fn routed<P: LifecyclePublication>(selected: Json, steps: &mut P) -> IvyResult<Json> {
    let mut outcome = f(&selected, "outcome").clone();
    if !f(&selected, "write").is_null() {
        outcome.insert(
            "task",
            steps
                .write(
                    "task-board/task",
                    f(f(&selected, "write"), "value"),
                    f(f(&selected, "write"), "destination"),
                )
                .await?,
        );
    }
    Ok(outcome)
}
impl PublicationRoutes<Json> for Bindings {
    async fn native_update<P: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut P,
        transcript: Option<&Json>,
    ) -> IvyResult<Json> {
        routed(
            self.0.call(
                "native",
                [
                    operation.pin.clone(),
                    operation.value.clone(),
                    transcript.cloned().unwrap_or(Json::Null),
                ],
            )?,
            steps,
        )
        .await
    }
    async fn lifecycle<P: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut P,
        outcome: Json,
    ) -> IvyResult<Json> {
        routed(
            self.0.call(
                "lifecycle",
                [operation.pin.clone(), operation.value.clone(), outcome],
            )?,
            steps,
        )
        .await
    }
    async fn existing_task<P: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut P,
        outcome: Json,
        normalized: Option<&JsString>,
    ) -> IvyResult<Json> {
        routed(
            self.0.call(
                "existing",
                [
                    operation.pin.clone(),
                    operation.value.clone(),
                    outcome,
                    normalized.cloned().map(Json::String).unwrap_or(Json::Null),
                ],
            )?,
            steps,
        )
        .await
    }
}
#[tokio::test(flavor = "current_thread")]
async fn root_publication_matches_original_dispatch_creation_configuration_plan_and_recovery() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    for row in data().as_array().unwrap() {
        let replay = Arc::new(Replay {
            name: s(row, "name").clone(),
            steps: Mutex::new(
                f(row, "boundaries")
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
        });
        let operations = Operations {
            store: World {
                replay: replay.clone(),
                root: f(row, "root").as_js_string().cloned(),
            },
            now: clock,
        };
        let bindings = Bindings(replay.clone());
        let callback = |pin: Json, task: Json| {
            let replay = replay.clone();
            Box::pin(async move { replay.call("taskWritten", [pin, task]).map(|_| ()) })
                as std::pin::Pin<Box<dyn std::future::Future<Output = IvyResult<()>>>>
        };
        let engine = EnginePublisher {
            operations: &operations,
            catalogs: &catalogs,
            checks: &bindings,
            routes: &bindings,
            task_written: &callback,
        };
        let mut outcomes = vec![];
        for call in f(row, "calls").as_array().unwrap() {
            outcomes.push(outcome(
                engine
                    .publish(
                        &document(f(call, "operation")),
                        (!f(call, "transcript").is_null()).then(|| f(call, "transcript")),
                        f(call, "normalized").as_js_string(),
                    )
                    .await,
            ));
        }
        let actual = Json::object([("outcomes", Json::Array(outcomes))]);
        assert_eq!(
            hash_json(&actual).unwrap(),
            s(row, "expectedHash").to_utf8().unwrap(),
            "{:?}: actual {actual:?}, expected {:?}",
            replay.name,
            f(row, "expected")
        );
        assert!(
            replay.steps.lock().unwrap().is_empty(),
            "{:?}: unconsumed original boundaries",
            replay.name
        );
    }
}
