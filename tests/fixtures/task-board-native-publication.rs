use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, hash::hash_json, json::parse,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_task_board::{
    native_intent::EvidenceIdentity,
    native_lifecycle::{LifecycleDocument, LifecyclePublication, LifecycleStore},
    native_publication::*,
    operations::{Document, Store},
};
use std::{
    collections::VecDeque,
    path::Path,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
};
#[path = "task-board-native-publication-journal.rs"]
mod journal;
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> &'a JsString {
    v.required(k).unwrap().as_js_string().unwrap()
}
fn doc(v: &Json) -> Document {
    Document {
        pin: f(v, "pin").clone(),
        value: f(v, "value").clone(),
        parent_id: f(f(v, "metadata"), "parentId").as_js_string().cloned(),
        content_hash: s(f(v, "revision"), "contentHash").to_utf8().unwrap(),
    }
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
pub(crate) fn data() -> &'static Json {
    static D: OnceLock<Json> = OnceLock::new();
    D.get_or_init(|| {
        let p = Command::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/task-board-native-publication-oracle.mjs",
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
fn transcript(v: &Json) -> Option<VerifiedPublicationTranscript> {
    if v == &Json::Null {
        return None;
    }
    let snapshot = f(v, "snapshot");
    let run = doc(f(f(snapshot, "source"), "run"));
    let value = ivy_task_board::native_turn_evidence::Transcript {
        document: doc(f(v, "document")),
        completion: ivy_task_board::native_turn_evidence::Completion {
            status: s(v, "status").clone(),
            summary: s(v, "summary").clone(),
            snapshot: ivy_task_board::native_turn_evidence::Snapshot {
                document: doc(f(snapshot, "document")),
                source: ivy_task_board::native_turn_evidence::Source {
                    run,
                    native_version: "native".into(),
                    thread_id: "thread".into(),
                    turn_id: "turn".into(),
                },
                thread: f(snapshot, "thread").clone(),
                turn: f(snapshot, "turn").clone(),
            },
        },
    };
    Some(VerifiedPublicationTranscript::from_verified(&value))
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
            .unwrap_or_else(|| panic!("Unexpected {kind} {:?}: {args:?}", self.name));
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
struct Client(Arc<Replay>);
impl RpcClient for Client {
    fn request<'a>(&'a self, m: &'a str, a: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move { self.0.call("rpc", [m.into(), a]) })
    }
}
struct World<'a> {
    row: &'a Json,
    replay: Arc<Replay>,
    client: Arc<Client>,
    principal: JsString,
}
impl Store for World<'_> {
    fn root_object_id(&self) -> Option<JsString> {
        Some("root".into())
    }
    fn contract_version(&self, _: &str) -> IvyResult<String> {
        panic!("Unexpected contract lookup")
    }
    fn retired_operation(&self, _: &JsString, _: &JsString) -> IvyResult<bool> {
        panic!("Unexpected replay lookup")
    }
    fn local_key(&self, id: &JsString) -> IvyResult<Option<String>> {
        self.replay
            .call("localKey", [Json::String(id.clone())])?
            .as_js_string()
            .map(JsString::to_utf8)
            .transpose()
    }
    fn complete_local_workflow(&self, _: &JsString, _: &JsString) -> IvyResult<()> {
        panic!("Missing publication dependency")
    }
    async fn named(&self, _: &str, _: &str, _: Option<&JsString>) -> IvyResult<Option<Document>> {
        panic!("Unexpected name lookup")
    }
    async fn read(&self, k: &str, a: &Json, p: bool) -> IvyResult<Document> {
        assert!(!p);
        Ok(doc(&self.replay.call("read", [k.into(), a.clone()])?))
    }
    async fn write(&self, _: &str, _: &Json, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("All writes must use retained steps")
    }
    async fn prepared_mutation_id(&self, _: &str) -> IvyResult<JsString> {
        panic!("All writes must use retained steps")
    }
    async fn write_prepared(&self, _: &str, _: &Json, _: &JsString, _: &Json) -> IvyResult<Json> {
        panic!("All writes must use retained steps")
    }
}
impl LifecycleStore for World<'_> {
    async fn lifecycle_read(
        &self,
        k: &str,
        a: &Json,
        p: bool,
        archived: bool,
    ) -> IvyResult<LifecycleDocument> {
        assert!(!p && !archived);
        let d = self.replay.call("read", [k.into(), a.clone()])?;
        Ok(LifecycleDocument {
            document: doc(&d),
            metadata: f(&d, "metadata").clone(),
        })
    }
    async fn page(&self, _: &str, _: &Json) -> IvyResult<Json> {
        panic!("Unexpected page")
    }
    async fn archive(&self, _: &Json) -> IvyResult<Json> {
        panic!("No native/archive effect is permitted")
    }
    async fn stat(&self, _: &JsString) -> IvyResult<Json> {
        panic!("Unexpected stat")
    }
    fn release_workspace(&self, id: &JsString) -> IvyResult<()> {
        self.replay.call("release", [Json::String(id.clone())])?;
        Ok(())
    }
}
impl NativePublicationStore for World<'_> {
    fn complete_after_publication(
        &self,
        r: &JsString,
        a: &JsString,
        o: &JsString,
    ) -> IvyResult<()> {
        self.replay.call(
            "complete",
            [
                Json::String(r.clone()),
                Json::String(a.clone()),
                Json::String(o.clone()),
            ],
        )?;
        Ok(())
    }
}
impl NativePublicationPolicies for World<'_> {
    fn now(&self) -> JsString {
        self.replay
            .call("now", [])
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone()
    }
    fn handoff(&self, t: &Json, r: &JsString) -> IvyResult<Json> {
        self.replay
            .call("handoff", [t.clone(), Json::String(r.clone())])
    }
    fn retry_at(&self, t: &Json, a: &JsString) -> IvyResult<Json> {
        self.replay
            .call("retry", [t.clone(), Json::String(a.clone())])
    }
    fn shared_project(&self, r: &Json, w: &Json) -> IvyResult<bool> {
        Ok(self.replay.call("shared", [r.clone(), w.clone()])? == Json::Bool(true))
    }
    async fn aggregate(&self, t: &Json, id: &JsString) -> IvyResult<()> {
        self.replay
            .call("aggregate", [t.clone(), Json::String(id.clone())])?;
        Ok(())
    }
    async fn finish_active<P: LifecyclePublication>(&self, c: &Json, _: &mut P) -> IvyResult<Json> {
        // The current completion policy is deliberately not a production default.
        // Bind the frozen source's saved projection, and verify every input boundary.
        let p = f(self.row, "policy");
        assert_ne!(
            p,
            &Json::Null,
            "Missing explicit policy: {:?}",
            self.replay.name
        );
        assert_eq!(
            hash_json(c).unwrap(),
            hash_json(f(p, "context")).unwrap(),
            "{:?}: current-policy context differs\nactual {c:?}\nexpected {:?}",
            self.replay.name,
            f(p, "context")
        );
        loop {
            let e = self.replay.steps.lock().unwrap().front().cloned();
            let Some(e) = e else {
                break;
            };
            if f(&e, "kind") != &Json::from("handoff") {
                break;
            }
            let a = f(&e, "args").as_array().unwrap();
            self.handoff(&a[0], a[1].as_js_string().unwrap())?;
        }
        Ok(f(p, "value").clone())
    }
}
impl NativePublicationPolicies for &World<'_> {
    fn now(&self) -> JsString {
        (**self).now()
    }
    fn handoff(&self, t: &Json, r: &JsString) -> IvyResult<Json> {
        (**self).handoff(t, r)
    }
    fn retry_at(&self, t: &Json, a: &JsString) -> IvyResult<Json> {
        (**self).retry_at(t, a)
    }
    fn shared_project(&self, r: &Json, w: &Json) -> IvyResult<bool> {
        (**self).shared_project(r, w)
    }
    async fn aggregate(&self, t: &Json, id: &JsString) -> IvyResult<()> {
        (**self).aggregate(t, id).await
    }
    async fn finish_active<P: LifecyclePublication>(&self, c: &Json, p: &mut P) -> IvyResult<Json> {
        (**self).finish_active(c, p).await
    }
}
impl NativePublicationEngine for World<'_> {
    fn principal_id(&self) -> &JsString {
        &self.principal
    }
    async fn workspace(&self, t: &Json, r: &Json) -> IvyResult<Json> {
        NativePublicationRuntime::new(self.client.clone(), self.principal.clone(), self)
            .workspace(t, r)
            .await
    }
}
impl NativePublicationEvidence for World<'_> {
    async fn snapshot(&self, r: &Json, a: &Json) -> IvyResult<Json> {
        self.replay.call("snapshot", [r.clone(), a.clone()])
    }
    async fn completion(&self, r: &Json, s: &Json, o: &Json) -> IvyResult<Json> {
        self.replay
            .call("completion", [r.clone(), s.clone(), o.clone()])
    }
    fn identity(&self, r: &Json, c: &Json, p: &JsString) -> IvyResult<EvidenceIdentity> {
        let v = self
            .replay
            .call("identity", [r.clone(), c.clone(), Json::String(p.clone())])?;
        Ok(EvidenceIdentity {
            operation_id: s(&v, "operationId").clone(),
            caller_principal_id: s(&v, "callerPrincipalId").clone(),
            service_node_id: s(&v, "serviceNodeId").clone(),
            native_version: s(&v, "nativeVersion").clone(),
            method: s(&v, "method").clone(),
            params: f(&v, "params").clone(),
        })
    }
    async fn read(&self, p: &Json, a: &Json, i: &EvidenceIdentity) -> IvyResult<Json> {
        self.replay
            .call("evidence", [p.clone(), a.clone(), i.to_json()])
    }
    async fn intended(&self, r: &Json, o: &Json) -> IvyResult<Json> {
        self.replay
            .call("intended", [r.clone(), "steer".into(), o.clone()])
    }
    fn resolve(&self, r: &Json) -> IvyResult<Json> {
        self.replay.call("resolve", [r.clone()])
    }
}
struct Steps(Arc<Replay>, Json);
impl LifecyclePublication for Steps {
    fn pin(&self) -> IvyResult<Json> {
        Ok(self.1.clone())
    }
    async fn write(&mut self, k: &str, v: &Json, d: &Json) -> IvyResult<Json> {
        self.0.call("write", [k.into(), v.clone(), d.clone()])
    }
    async fn create(&mut self, k: &str, v: &Json, l: &JsString) -> IvyResult<Json> {
        self.0
            .call("create", [k.into(), v.clone(), Json::String(l.clone())])
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_publication_matches_original_saved_observations_and_ordered_effects() {
    for row in data().as_array().unwrap() {
        let replay = Arc::new(Replay {
            steps: Mutex::new(
                f(row, "boundaries")
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
            name: s(row, "name").clone(),
        });
        let world = World {
            row,
            replay: replay.clone(),
            client: Arc::new(Client(replay.clone())),
            principal: "board".into(),
        };
        let op = doc(f(f(row, "initial"), "operation"));
        let mut steps = Steps(replay.clone(), op.pin.clone());
        let verified = transcript(f(row, "verified"));
        let actual = outcome(
            NativePublisher {
                store: &world,
                engine: &world,
                evidence: &world,
            }
            .publish(&op, &mut steps, verified.as_ref())
            .await,
        );
        assert_eq!(
            hash_json(&actual).unwrap(),
            s(row, "expectedHash").to_utf8().unwrap(),
            "{:?}: actual {actual:?}, expected {:?}",
            replay.name,
            f(row, "expected")
        );
        let pending = replay.steps.lock().unwrap();
        assert!(
            pending.is_empty(),
            "{:?}: unconsumed effects {:?}",
            replay.name,
            pending.front()
        );
    }
}
