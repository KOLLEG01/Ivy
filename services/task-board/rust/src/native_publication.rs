//! Publish saved original-owner observations through the retained write program.
//! Current completion/question/handoff/check policy is a required engine binding.
use crate::{
    journal_store::Contracts,
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    local_retention::LocalRetention,
    native_evidence::NativeEvidence,
    native_intent::{EvidenceIdentity, NativeIntent, NativeSlot, TaskPrompts},
    native_lifecycle::{LifecyclePublication, LifecycleStore},
    native_state::{finished_run, project_native_state},
    native_turn_evidence::{
        Completion as TurnCompletion, Snapshot, Transcript, TurnEvidence, TurnEvidenceStore,
    },
    operations::{Document, Store},
    read_evidence::ReadEvidenceStore,
    turn_output::bounded_output_summary,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{rpc::RpcClient, tools::ToolClients};
use std::{collections::HashSet, sync::Arc};

/// The dependency keeps closed Run/call/plan graphs until publication finishes.
pub trait NativePublicationStore: LifecycleStore {
    fn complete_after_publication(
        &self,
        root: &JsString,
        at: &JsString,
        operation: &JsString,
    ) -> IvyResult<()>;
}
impl<
    R: RpcClient + 'static,
    P: Contracts,
    C: Fn() -> String,
    I: Fn() -> JsString,
    Q: LifecyclePages,
> NativePublicationStore for LifecycleJournal<'_, '_, R, P, C, I, Q>
{
    fn complete_after_publication(
        &self,
        root: &JsString,
        at: &JsString,
        operation: &JsString,
    ) -> IvyResult<()> {
        LocalRetention {
            journal: crate::local_journal::LocalJournal {
                connection: self.journal.journal.connection,
                root_object_id: self.journal.journal.root_object_id.clone(),
            },
            client: self.journal.client.as_ref(),
            limits: crate::local_retention::HistoryLimits {
                count: self.journal.limits.count,
                bytes: self.journal.limits.bytes,
            },
        }
        .complete_workflow(root, at, Some(operation))
    }
}

#[allow(async_fn_in_trait)]
pub trait NativePublicationPolicies {
    fn now(&self) -> JsString;
    fn handoff(&self, task: &Json, run: &JsString) -> IvyResult<Json>;
    fn retry_at(&self, turn: &Json, at: &JsString) -> IvyResult<Json>;
    fn shared_project(&self, requirement: &Json, workspace: &Json) -> IvyResult<bool>;
    async fn aggregate(&self, task: &Json, id: &JsString) -> IvyResult<()>;
    /// Apply the accepted current completion policy to the active Task. Any
    /// additional records (including notifications) use these same retained steps.
    /// Context retains the original Task/Run, terminal Run, result pins and time.
    async fn finish_active<P: LifecyclePublication>(
        &self,
        context: &Json,
        steps: &mut P,
    ) -> IvyResult<Json>;
}
#[allow(async_fn_in_trait)]
pub trait NativePublicationEngine: NativePublicationPolicies {
    fn principal_id(&self) -> &JsString;
    async fn workspace(&self, task: &Json, run: &Json) -> IvyResult<Json>;
}
pub struct NativePublicationRuntime<R, P> {
    pub client: Arc<R>,
    pub principal_id: JsString,
    pub policies: P,
    tools: Arc<ToolClients>,
}
impl<R: RpcClient + 'static, P> NativePublicationRuntime<R, P> {
    pub fn new(client: Arc<R>, principal_id: JsString, policies: P) -> Self {
        Self {
            tools: Arc::new(ToolClients::new(client.clone())),
            client,
            principal_id,
            policies,
        }
    }
}
impl<R, P: NativePublicationPolicies> NativePublicationPolicies for NativePublicationRuntime<R, P> {
    fn now(&self) -> JsString {
        self.policies.now()
    }
    fn handoff(&self, t: &Json, r: &JsString) -> IvyResult<Json> {
        self.policies.handoff(t, r)
    }
    fn retry_at(&self, t: &Json, a: &JsString) -> IvyResult<Json> {
        self.policies.retry_at(t, a)
    }
    fn shared_project(&self, r: &Json, w: &Json) -> IvyResult<bool> {
        self.policies.shared_project(r, w)
    }
    async fn aggregate(&self, t: &Json, id: &JsString) -> IvyResult<()> {
        self.policies.aggregate(t, id).await
    }
    async fn finish_active<W: LifecyclePublication>(&self, c: &Json, w: &mut W) -> IvyResult<Json> {
        self.policies.finish_active(c, w).await
    }
}
impl<R: RpcClient + 'static, P: NativePublicationPolicies> NativePublicationEngine
    for NativePublicationRuntime<R, P>
{
    fn principal_id(&self) -> &JsString {
        &self.principal_id
    }
    async fn workspace(&self, t: &Json, r: &Json) -> IvyResult<Json> {
        crate::native_inspector::management_read(
            &self.tools,
            s(f(r, "target"), "serviceNodeId")?,
            "resolveWorkspace",
            Json::object([
                ("taskKey", f(t, "taskKey").clone()),
                (
                    "requirement",
                    f(f(t, "fields"), "workspaceRequirement").clone(),
                ),
                ("prepare", false.into()),
                ("verifyOrigin", true.into()),
            ]),
        )
        .await
    }
}

/// Evidence verification runs against the same admitted journal/catalogs. No
/// native dispatch is performed by these methods while publication holds a gate.
#[allow(async_fn_in_trait)]
pub trait NativePublicationEvidence {
    async fn snapshot(&self, run: &Json, artifact: &Json) -> IvyResult<Json>;
    async fn completion(&self, run: &Json, snapshot: &Json, output: &Json) -> IvyResult<Json>;
    fn identity(
        &self,
        run: &Json,
        call: &Json,
        principal: &JsString,
    ) -> IvyResult<EvidenceIdentity>;
    async fn read(
        &self,
        parent: &Json,
        artifact: &Json,
        identity: &EvidenceIdentity,
    ) -> IvyResult<Json>;
    async fn intended(&self, run: &Json, origin: &Json) -> IvyResult<Json>;
    fn resolve(&self, request: &Json) -> IvyResult<Json>;
}
pub struct PublicationEvidence<'a, S, P> {
    pub store: &'a S,
    pub catalogs: &'a CheckedNativeContracts,
    pub principal_id: &'a JsString,
    pub prompts: &'a P,
}
fn document(v: &Document) -> Json {
    Json::object([
        ("pin", v.pin.clone()),
        ("value", v.value.clone()),
        (
            "revision",
            Json::object([("contentHash", v.content_hash.clone().into())]),
        ),
    ])
}
fn snapshot_value(v: &Snapshot) -> Json {
    Json::object([
        ("document", document(&v.document)),
        ("source", Json::object([("run", document(&v.source.run))])),
        ("thread", v.thread.clone()),
        ("turn", v.turn.clone()),
    ])
}
fn completion_value(v: &TurnCompletion) -> Json {
    Json::object([
        ("snapshot", snapshot_value(&v.snapshot)),
        ("status", Json::String(v.status.clone())),
        ("summary", Json::String(v.summary.clone())),
    ])
}
/// Constructed from an evidence graph already verified outside the gate, as in
/// the original engine. Wire inputs cannot construct this private payload.
pub struct VerifiedPublicationTranscript(Json);
impl VerifiedPublicationTranscript {
    pub fn from_verified(transcript: &Transcript) -> Self {
        let mut v = completion_value(&transcript.completion);
        v.insert("document", document(&transcript.document));
        Self(v)
    }
}
impl<S: Store + ReadEvidenceStore + TurnEvidenceStore, P: TaskPrompts> NativePublicationEvidence
    for PublicationEvidence<'_, S, P>
{
    async fn snapshot(&self, r: &Json, a: &Json) -> IvyResult<Json> {
        Ok(snapshot_value(
            &TurnEvidence {
                store: self.store,
                catalogs: self.catalogs,
                principal_id: self.principal_id,
            }
            .snapshot(r, a)
            .await?,
        ))
    }
    async fn completion(&self, r: &Json, s: &Json, o: &Json) -> IvyResult<Json> {
        Ok(completion_value(
            &TurnEvidence {
                store: self.store,
                catalogs: self.catalogs,
                principal_id: self.principal_id,
            }
            .completion(r, s, o)
            .await?,
        ))
    }
    fn identity(&self, r: &Json, c: &Json, p: &JsString) -> IvyResult<EvidenceIdentity> {
        NativeIntent {
            store: self.store,
            catalogs: self.catalogs,
        }
        .evidence_identity(r, c, p)
    }
    async fn read(&self, p: &Json, a: &Json, i: &EvidenceIdentity) -> IvyResult<Json> {
        NativeEvidence {
            store: self.store,
            catalogs: self.catalogs,
        }
        .read(p, a, i)
        .await
    }
    async fn intended(&self, r: &Json, o: &Json) -> IvyResult<Json> {
        NativeIntent {
            store: self.store,
            catalogs: self.catalogs,
        }
        .intended_call(r, NativeSlot::Steer, Some(o), self.prompts)
        .await
    }
    fn resolve(&self, r: &Json) -> IvyResult<Json> {
        NativeIntent {
            store: self.store,
            catalogs: self.catalogs,
        }
        .resolve_native_request(r)
    }
}

pub struct NativePublisher<'a, S, E, V> {
    pub store: &'a S,
    pub engine: &'a E,
    pub evidence: &'a V,
}
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn eq(v: &Json, k: &str, t: &str) -> bool {
    f(v, k) == &Json::from(t)
}
fn has(v: &Json, k: &str) -> bool {
    v.get(k).is_some()
}
fn array(v: &Json) -> &[Json] {
    v.as_array().unwrap_or(&[])
}
fn text(v: &Json) -> JsString {
    v.as_js_string().cloned().unwrap_or_default()
}
fn truthy(v: &Json) -> bool {
    match v {
        Json::Null => false,
        Json::Bool(x) => *x,
        Json::Number(n) => *n != 0.0 && !n.is_nan(),
        Json::String(x) => !x.0.is_empty(),
        _ => true,
    }
}
fn n(v: &Json, k: &str) -> f64 {
    f(v, k).as_f64().unwrap_or(f64::NAN)
}
fn patch(v: &Json, fields: impl IntoIterator<Item = (&'static str, Json)>) -> Json {
    let mut v = v.clone();
    for (k, x) in fields {
        v.insert(k, x);
    }
    v
}
fn same(a: &Json, b: &Json) -> IvyResult<bool> {
    Ok(canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?)
}
fn require(b: bool, c: &str, m: &str) -> IvyResult<()> {
    if b { Ok(()) } else { Err(IvyError::new(c, m)) }
}
fn one(v: &Json, k: &str, values: &[&str]) -> bool {
    values.iter().any(|x| eq(v, k, x))
}
fn wait(at: &Json, reason: &str, detail: Json) -> Json {
    Json::object([
        ("reason", reason.into()),
        ("detail", detail),
        ("since", at.clone()),
    ])
}
fn join(parts: &[JsString], separator: &str) -> JsString {
    let mut out = JsString::default();
    for (i, p) in parts.iter().enumerate() {
        if i != 0 {
            out.0.extend(JsString::from(separator).0);
        }
        out.0.extend_from_slice(&p.0);
    }
    out
}
fn summary(turn: &Json, status: &JsString) -> JsString {
    let error = text(f(f(turn, "error"), "message"));
    if status == &JsString::from("failed") && !error.0.is_empty() {
        return bounded_output_summary(&error);
    }
    let messages: Vec<_> = array(f(turn, "items"))
        .iter()
        .filter(|v| eq(v, "type", "agentMessage"))
        .collect();
    let finals: Vec<_> = messages
        .iter()
        .copied()
        .filter(|v| eq(v, "phase", "final_answer"))
        .collect();
    let selected = if finals.is_empty() {
        messages.last().copied().into_iter().collect()
    } else {
        finals
    };
    let joined = join(
        &selected
            .iter()
            .map(|v| text(f(v, "text")))
            .collect::<Vec<_>>(),
        "\n\n",
    );
    bounded_output_summary(&if joined.0.is_empty() {
        join(&["Native turn ".into(), status.clone(), ".".into()], "")
    } else {
        joined
    })
}
fn read_outcome(run: &Json, status: &JsString) -> IvyResult<Json> {
    let prior = f(run, "externalOutcome");
    let state = if status == &JsString::from("completed") {
        "succeeded"
    } else if status == &JsString::from("failed") {
        "failed"
    } else if status == &JsString::from("interrupted") {
        if eq(prior, "state", "failed") {
            "failed"
        } else if truthy(f(run, "cancellation")) {
            "cancelled"
        } else {
            "failed"
        }
    } else {
        "active"
    };
    let code = if ["inProgress", "completed"]
        .iter()
        .any(|x| status == &JsString::from(*x))
    {
        Json::Null
    } else {
        Json::String(join(&["native_".into(), status.clone()], ""))
    };
    require(
        !one(prior, "state", &["succeeded", "failed", "cancelled"])
            || eq(prior, "state", state) && f(prior, "code") == &code,
        "task_board_native_state_changed",
        "A native read cannot regress or reclassify an already known terminal outcome.",
    )?;
    Ok(Json::object([("state", state.into()), ("code", code)]))
}
fn completion(
    state: &str,
    summary: Json,
    evidence: &Json,
    code: Json,
    retry: Option<Json>,
) -> Json {
    let mut v = Json::object([
        ("state", state.into()),
        ("summary", summary),
        ("evidence", evidence.clone()),
        ("code", code),
    ]);
    if let Some(r) = retry {
        v.insert("retryAt", r);
    }
    v
}
fn run_artifact(d: &Document) -> Json {
    Json::object([
        ("object", d.pin.clone()),
        ("label", "Saved native attempt before closure".into()),
        ("mediaType", "application/json".into()),
        ("contentHash", d.content_hash.clone().into()),
    ])
}
fn outcome(id: &Json) -> Json {
    Json::object([
        ("operationId", id.clone()),
        ("task", Json::Null),
        ("plan", Json::Null),
        ("run", Json::Null),
        ("history", Json::Null),
        ("result", Json::Null),
        ("review", Json::Null),
        ("attachment", Json::Null),
    ])
}

impl<S: NativePublicationStore, E: NativePublicationEngine, V: NativePublicationEvidence>
    NativePublisher<'_, S, E, V>
{
    async fn observed(&self, run: &Json, pin: &Json) -> IvyResult<(Document, Json)> {
        let call = self
            .store
            .read("task-board/native-call", pin, false)
            .await?;
        let v = &call.value;
        require(
            truthy(f(v, "evidence"))
                && truthy(f(v, "observedAt"))
                && truthy(f(v, "ownerPhase"))
                && !eq(v, "state", "prepared"),
            "task_board_native_evidence_required",
            "The NativeCall has no saved owner observation.",
        )?;
        let identity = self.evidence.identity(run, v, self.engine.principal_id())?;
        let observation = self.evidence.read(pin, f(v, "evidence"), &identity).await?;
        require(
            f(v, "ownerPhase") == f(&observation, "phase")
                && f(v, "epoch") == f(&observation, "epoch")
                && f(v, "code") == f(&observation, "code"),
            "task_board_evidence_mismatch",
            "NativeCall observation fields must agree with its verified owner evidence.",
        )?;
        Ok((call, observation))
    }
    pub async fn publish<P: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut P,
        verified_transcript: Option<&VerifiedPublicationTranscript>,
    ) -> IvyResult<Json> {
        let op = &operation.value;
        let request = f(op, "request");
        let change = f(request, "change");
        require(
            eq(request, "action", "nativeUpdate")
                && eq(f(op, "actor"), "source", "native")
                && f(op, "callerPrincipalId") == &Json::String(self.engine.principal_id().clone()),
            "task_board_forbidden",
            "Only the authenticated logical owner can publish native observations.",
        )?;
        let at = f(op, "createdAt");
        let task_id = s(request, "taskId")?;
        let task = self
            .store
            .lifecycle_read(
                "task-board/task",
                &Json::object([
                    ("objectId", Json::String(task_id.clone())),
                    ("revision", f(request, "expectedRevision").clone()),
                ]),
                false,
                false,
            )
            .await?;
        let original = self
            .store
            .lifecycle_read("task-board/run", f(request, "run"), false, false)
            .await?;
        let task_value = &task.document.value;
        let run = &original.document.value;
        let run_pin = f(request, "run");
        let run_id = s(run_pin, "objectId")?;
        let active = f(f(f(task_value, "claim"), "run"), "objectId") == f(run_pin, "objectId");
        if array(f(op, "writes")).is_empty() {
            require(
                f(&task.metadata, "currentRevision") == f(request, "expectedRevision")
                    && !truthy(f(task_value, "publication"))
                    && f(&original.metadata, "currentRevision") == f(run_pin, "revision"),
                "revision_conflict",
                "Task or Run changed before native publication.",
            )?;
            self.engine.aggregate(task_value, task_id).await?;
            require(
                f(run, "taskId") == f(request, "taskId")
                    && f(f(run, "originalRequest"), "taskId") == f(request, "taskId")
                    && f(run, "operationId") == f(f(run, "originalRequest"), "operationId")
                    && !finished_run(run),
                "task_board_run_mismatch",
                "The native observation must identify this unfinished original execution attempt.",
            )?;
            if active {
                require(
                    same(f(f(task_value, "claim"), "run"), run_pin)?,
                    "revision_conflict",
                    "Native publication requires the exact current claimed Run revision.",
                )?;
            }
        }
        let mut next = patch(
            run,
            [
                ("calls", f(run, "calls").clone()),
                ("updatedAt", at.clone()),
            ],
        );
        let mut done = Json::Null;
        let mut waiting = Json::Null;
        let mut detail = Json::from("");
        let mut kind = "recovered";
        let mut steered: Vec<Json> = vec![];
        match s(change, "kind")?
            .to_utf8()
            .map_err(|_| IvyError::internal())?
            .as_str()
        {
            "readSnapshot" => {
                let snapshot = self
                    .evidence
                    .snapshot(run_pin, f(change, "snapshot"))
                    .await?;
                require(
                    n(change, "notificationCursor") >= n(run, "notificationCursor"),
                    "task_board_cursor_regression",
                    "Native notification progress cannot move backwards.",
                )?;
                let projected = project_native_state(
                    run,
                    s(f(f(&snapshot, "document"), "value"), "status")?,
                    f(f(f(&snapshot, "document"), "value"), "epoch"),
                    f(&snapshot, "thread"),
                    s(op, "createdAt")?,
                    f(change, "snapshot"),
                )?;
                next = patch(
                    &next,
                    [
                        (
                            "notificationCursor",
                            f(change, "notificationCursor").clone(),
                        ),
                        ("phase", f(&projected, "phase").clone()),
                        ("externalOutcome", f(&projected, "externalOutcome").clone()),
                    ],
                );
                waiting = f(&projected, "waiting").clone();
                detail = "Saved the exact native turn state with separate read evidence.".into();
            }
            "completion" | "transcript" => {
                let transcript = eq(change, "kind", "transcript");
                let verified = if transcript {
                    let v = verified_transcript.map(|v| &v.0);
                    require(
                        v.is_some_and(|v| {
                            f(f(v, "document"), "pin") == f(f(change, "evidence"), "object")
                                && f(f(f(v, "document"), "revision"), "contentHash")
                                    == f(f(change, "evidence"), "contentHash")
                                && f(f(f(f(v, "snapshot"), "source"), "run"), "pin") == run_pin
                        }),
                        "task_board_native_evidence_required",
                        "Transcript publication requires its exact immutable evidence graph verified before gate acquisition.",
                    )?;
                    v.unwrap().clone()
                } else {
                    self.evidence
                        .completion(run_pin, f(change, "snapshot"), f(change, "output"))
                        .await?
                };
                require(
                    n(change, "notificationCursor") >= n(run, "notificationCursor"),
                    "task_board_cursor_regression",
                    "Native notification progress cannot move backwards.",
                )?;
                next.insert(
                    "notificationCursor",
                    f(change, "notificationCursor").clone(),
                );
                let state = read_outcome(run, s(&verified, "status")?)?;
                require(
                    one(&state, "state", &["succeeded", "failed", "cancelled"]),
                    "task_board_native_state_changed",
                    if transcript {
                        "A transcript must identify a terminal native outcome."
                    } else {
                        "Completion must identify a terminal native outcome."
                    },
                )?;
                let handoff = if transcript {
                    Json::Null
                } else {
                    self.engine.handoff(task_value, run_id)?
                };
                let turn = f(f(&verified, "snapshot"), "turn");
                let body = if eq(&verified, "status", "failed")
                    && truthy(f(f(turn, "error"), "message"))
                {
                    Json::String(summary(turn, s(&verified, "status")?))
                } else if transcript {
                    f(&verified, "summary").clone()
                } else {
                    if truthy(&handoff) {
                        f(&handoff, "body").clone()
                    } else {
                        f(&verified, "summary").clone()
                    }
                };
                done = completion(
                    &f(&state, "state").as_str()?,
                    body,
                    f(change, if transcript { "evidence" } else { "snapshot" }),
                    f(&state, "code").clone(),
                    Some(self.engine.retry_at(turn, s(op, "createdAt")?)?),
                );
            }
            "reattachCall" => {
                require(
                    active
                        && !truthy(f(run, "cancellation"))
                        && truthy(f(run, "primaryResourceRef"))
                        && !truthy(f(run, "turnId"))
                        && truthy(f(f(run, "calls"), "thread")),
                    "task_board_native_order",
                    "Pre-turn reattachment requires the active uncancelled claim and confirmed primary.",
                )?;
                let call = self
                    .store
                    .read("task-board/native-call", f(change, "call"), false)
                    .await?;
                let origin = f(&call.value, "origin");
                let previous_pin = f(f(run, "calls"), "thread");
                require(
                    eq(&call.value, "state", "prepared")
                        && eq(origin, "kind", "reattach")
                        && same(f(origin, "predecessor"), previous_pin)?
                        && f(f(change, "call"), "objectId") != f(previous_pin, "objectId"),
                    "task_board_native_intent_mismatch",
                    "Reattachment must be a new prepared call for a replacement epoch with the exact retained predecessor.",
                )?;
                let (prior, observed) = self.observed(run, previous_pin).await?;
                let epoch = f(&observed, "epoch");
                require(
                    truthy(epoch) && f(origin, "preparedEpoch") != epoch,
                    "task_board_native_intent_mismatch",
                    "Reattachment requires a replacement native owner epoch.",
                )?;
                require(
                    (eq(&observed, "phase", "succeeded") && epoch == f(run, "nativeEpoch")
                        || eq(&observed, "phase", "outcome_unknown")
                            && eq(f(&prior.value, "request"), "method", "thread/resume")
                            && (!truthy(f(run, "nativeEpoch"))
                                || eq(f(&prior.value, "origin"), "kind", "reattach")))
                        && f(&call.value, "operationId") != f(&prior.value, "operationId"),
                    "task_board_native_order",
                    "Reattachment needs the previously confirmed thread operation and a distinct successor identity.",
                )?;
                if truthy(f(f(run, "calls"), "turn")) {
                    let pin = f(f(run, "calls"), "turn");
                    let turn = self
                        .store
                        .read("task-board/native-call", f(pin, "objectId"), false)
                        .await?;
                    require(
                        same(&turn.pin, pin)?
                            && eq(&turn.value, "state", "prepared")
                            && f(&turn.value, "ownerPhase") == &Json::Null
                            && f(&turn.value, "evidence") == &Json::Null,
                        "task_board_outcome_unresolved",
                        "An observed turn operation cannot be replaced or treated as unstarted by reattachment.",
                    )?;
                }
                next.insert(
                    "calls",
                    patch(f(&next, "calls"), [("thread", f(change, "call").clone())]),
                );
                next.insert("phase", "starting".into());
                detail="Attached a retained resume intent for the saved primary on a replacement native owner.".into();
            }
            "attachCall" => {
                let slot = s(change, "slot")?;
                let slot_text = slot.to_utf8().map_err(|_| IvyError::internal())?;
                let call = self
                    .store
                    .read("task-board/native-call", f(change, "call"), false)
                    .await?;
                let origin = f(&call.value, "origin");
                if slot_text == "steer" {
                    require(
                        active
                            && !truthy(f(run, "cancellation"))
                            && truthy(f(run, "turnId"))
                            && eq(&call.value, "state", "prepared")
                            && eq(origin, "kind", "steer")
                            && same(f(origin, "predecessor"), f(f(run, "calls"), "steer"))?,
                        "task_board_native_order",
                        "Steering requires the active turn and exact predecessor.",
                    )?;
                    if truthy(f(f(run, "calls"), "steer")) {
                        let (_, o) = self.observed(run, f(f(run, "calls"), "steer")).await?;
                        require(
                            one(&o, "phase", &["succeeded", "failed"]),
                            "task_board_outcome_unresolved",
                            "Resolve the original steering receipt before preparing another.",
                        )?;
                    }
                    require(
                        f(origin, "workRevision") == f(task_value, "workRevision")
                            && array(f(origin, "commentIds")).iter().all(|id| {
                                array(f(task_value, "commentDeliveries"))
                                    .iter()
                                    .any(|d| f(d, "commentId") == id && eq(d, "state", "queued"))
                            }),
                        "task_board_native_intent_mismatch",
                        "Steering must identify current queued input.",
                    )?;
                    let intended = self.evidence.intended(run, origin).await?;
                    require(
                        same(
                            &self.evidence.resolve(f(&call.value, "request"))?,
                            f(&intended, "request"),
                        )? && f(&call.value, "expectedDefinitionHash")
                            == f(&intended, "expectedDefinitionHash"),
                        "task_board_native_intent_mismatch",
                        "Steering must contain only the ticket reference and target the exact turn.",
                    )?;
                    next.insert(
                        "calls",
                        patch(f(&next, "calls"), [("steer", f(change, "call").clone())]),
                    );
                    waiting = f(task_value, "waiting").clone();
                    detail = "Prepared a ticket update for the active turn.".into();
                } else {
                    require(
                        !truthy(origin) || eq(origin, "kind", "initial"),
                        "task_board_native_order",
                        "A successor thread resume requires the explicit reattachment transition.",
                    )?;
                    require(
                        !truthy(f(f(run, "calls"), &slot_text))
                            && eq(&call.value, "state", "prepared")
                            && (active
                                || slot_text == "interrupt" && truthy(f(run, "cancellation"))),
                        "task_board_claim_inactive",
                        "Only this current claim may attach a new execution call; retained cancellation may still interrupt its prior turn.",
                    )?;
                    if slot_text == "turn" {
                        require(
                            !truthy(f(run, "cancellation")) && truthy(f(f(run, "calls"), "thread")),
                            "task_board_native_order",
                            "A turn requires thread confirmation and no pending cancellation.",
                        )?;
                        let (_, o) = self.observed(run, f(f(run, "calls"), "thread")).await?;
                        require(
                            eq(&o, "phase", "succeeded"),
                            "task_board_native_order",
                            "The primary thread operation is not conclusively successful.",
                        )?;
                    }
                    if slot_text == "thread" {
                        require(
                            !truthy(f(run, "cancellation")),
                            "task_board_cancellation_pending",
                            "Cancellation prevents preparation of a new native context.",
                        )?;
                    }
                    let mut calls = f(&next, "calls").clone();
                    calls.insert(slot_text.clone(), f(change, "call").clone());
                    next.insert("calls", calls);
                    detail = Json::String(join(
                        &[
                            "Attached exact native ".into(),
                            slot.clone(),
                            " intent before dispatch.".into(),
                        ],
                        "",
                    ));
                }
            }
            "observeCall" => {
                let slot = s(change, "slot")?;
                let slot_text = slot.to_utf8().map_err(|_| IvyError::internal())?;
                let prior = f(f(run, "calls"), &slot_text);
                require(
                    truthy(prior)
                        && f(prior, "objectId") == f(f(change, "call"), "objectId")
                        && n(f(change, "call"), "revision") > n(prior, "revision"),
                    "task_board_native_intent_mismatch",
                    "Observe a newer revision of the original attached native call.",
                )?;
                let (call, observation) = self.observed(run, f(change, "call")).await?;
                let original = self
                    .store
                    .read("task-board/native-call", prior, false)
                    .await?;
                let artifact = f(&call.value, "evidence");
                require(
                    !one(&original.value, "ownerPhase", &["succeeded", "failed"]),
                    "task_board_native_state_conflict",
                    "A conclusive native call observation cannot be rewritten or regressed.",
                )?;
                let initial = Json::object([("kind", "initial".into())]);
                let origin = |v: &Json| {
                    if truthy(f(v, "origin")) {
                        f(v, "origin").clone()
                    } else {
                        initial.clone()
                    }
                };
                require(
                    f(&call.value, "operationId") == f(&original.value, "operationId")
                        && f(&call.value, "preparedAt") == f(&original.value, "preparedAt")
                        && same(f(&call.value, "request"), f(&original.value, "request"))?
                        && same(&origin(&call.value), &origin(&original.value))?,
                    "task_board_native_intent_mismatch",
                    "Call observation cannot rewrite its original identity, origin or preparation.",
                )?;
                let mut calls = f(&next, "calls").clone();
                calls.insert(slot_text.clone(), f(change, "call").clone());
                next.insert("calls", calls);
                detail = Json::String(join(
                    &[
                        "Native ".into(),
                        slot.clone(),
                        " operation observed as ".into(),
                        text(f(&observation, "phase")),
                        ".".into(),
                    ],
                    "",
                ));
                if slot_text == "steer" {
                    waiting = f(task_value, "waiting").clone();
                    if eq(&observation, "phase", "succeeded") {
                        let reply = f(&observation, "reply");
                        require(
                            truthy(reply)
                                && has(reply, "result")
                                && f(f(reply, "result"), "turnId") == f(run, "turnId")
                                && f(&observation, "epoch") == f(run, "nativeEpoch")
                                && eq(f(&call.value, "origin"), "kind", "steer"),
                            "task_board_turn_mismatch",
                            "Steering acknowledgment must identify the original native turn and epoch.",
                        )?;
                        steered = array(f(f(&call.value, "origin"), "commentIds")).to_vec();
                    }
                } else if one(
                    &observation,
                    "phase",
                    &["accepted", "dispatched", "outcome_unknown"],
                ) {
                    next = patch(
                        &next,
                        [
                            ("phase", "outcome_unknown".into()),
                            (
                                "externalOutcome",
                                Json::object([
                                    ("state", "unknown".into()),
                                    ("observedAt", at.clone()),
                                    ("evidence", artifact.clone()),
                                    (
                                        "code",
                                        if f(&observation, "code") == &Json::Null {
                                            "native_operation_pending".into()
                                        } else {
                                            f(&observation, "code").clone()
                                        },
                                    ),
                                ]),
                            ),
                        ],
                    );
                    waiting = wait(at, "external_outcome", detail.clone());
                    kind = "outcome_unknown";
                } else if eq(&observation, "phase", "failed") {
                    if slot_text == "interrupt" {
                        next = patch(
                            &next,
                            [
                                ("phase", "cancel_requested".into()),
                                (
                                    "externalOutcome",
                                    patch(
                                        f(run, "externalOutcome"),
                                        [
                                            ("state", "unknown".into()),
                                            ("evidence", artifact.clone()),
                                            ("observedAt", at.clone()),
                                            (
                                                "code",
                                                if f(&observation, "code") == &Json::Null {
                                                    "native_interrupt_failed".into()
                                                } else {
                                                    f(&observation, "code").clone()
                                                },
                                            ),
                                        ],
                                    ),
                                ),
                            ],
                        );
                        waiting=wait(at,"external_outcome","Interruption failed; the native turn still requires terminal observation.".into());
                    } else {
                        let reply = f(&observation, "reply");
                        let reason = if truthy(reply) && has(reply, "error") {
                            let mut x = text(f(f(reply, "error"), "message"));
                            x.0.truncate(16384);
                            Json::String(x)
                        } else if f(&observation, "code") != &Json::Null {
                            f(&observation, "code").clone()
                        } else {
                            "No native success was recorded.".into()
                        };
                        done = completion(
                            "failed",
                            Json::String(join(
                                &[
                                    "Native ".into(),
                                    slot.clone(),
                                    " operation failed.\n\n".into(),
                                    text(&reason),
                                ],
                                "",
                            )),
                            artifact,
                            if f(&observation, "code") == &Json::Null {
                                "native_operation_failed".into()
                            } else {
                                f(&observation, "code").clone()
                            },
                            None,
                        );
                    }
                } else {
                    let reply = f(&observation, "reply");
                    require(
                        truthy(reply) && has(reply, "result") && truthy(f(&observation, "epoch")),
                        "task_board_native_evidence_required",
                        "A successful native call needs its original reply and epoch.",
                    )?;
                    let result = f(reply, "result");
                    if slot_text == "thread" {
                        let thread = f(result, "thread");
                        let id = text(f(thread, "id"));
                        require(
                            !id.0.is_empty() && f(thread, "ephemeral") != &Json::Bool(true),
                            "task_board_primary_mismatch",
                            "Native context creation must return a durable primary identity.",
                        )?;
                        if eq(f(&call.value, "request"), "method", "thread/resume") {
                            let resolved = self.evidence.resolve(f(&call.value, "request"))?;
                            require(
                                id == text(f(f(&resolved, "params"), "threadId")),
                                "task_board_primary_mismatch",
                                "Resume returned another primary context.",
                            )?;
                        }
                        next = patch(
                            &next,
                            [
                                (
                                    "primaryResourceRef",
                                    Json::object([
                                        (
                                            "serviceNodeId",
                                            f(f(run, "target"), "serviceNodeId").clone(),
                                        ),
                                        ("namespace", "codex".into()),
                                        ("kind", "thread".into()),
                                        ("nativeId", Json::String(id)),
                                    ]),
                                ),
                                ("nativeEpoch", f(&observation, "epoch").clone()),
                                (
                                    "phase",
                                    if truthy(f(run, "cancellation")) {
                                        "cancel_requested".into()
                                    } else {
                                        "starting".into()
                                    },
                                ),
                                (
                                    "externalOutcome",
                                    Json::object([
                                        ("state", "not_started".into()),
                                        ("observedAt", at.clone()),
                                        ("evidence", artifact.clone()),
                                        ("code", Json::Null),
                                    ]),
                                ),
                            ],
                        );
                    } else if slot_text == "turn" {
                        let id = text(f(f(result, "turn"), "id"));
                        require(
                            !id.0.is_empty(),
                            "task_board_turn_mismatch",
                            "Turn start must return its exact native identity.",
                        )?;
                        next = patch(
                            &next,
                            [
                                ("turnId", Json::String(id)),
                                ("nativeEpoch", f(&observation, "epoch").clone()),
                                (
                                    "phase",
                                    if truthy(f(run, "cancellation")) {
                                        "cancel_requested".into()
                                    } else {
                                        "running".into()
                                    },
                                ),
                                (
                                    "externalOutcome",
                                    Json::object([
                                        ("state", "active".into()),
                                        ("observedAt", at.clone()),
                                        ("evidence", artifact.clone()),
                                        ("code", Json::Null),
                                    ]),
                                ),
                            ],
                        );
                    } else {
                        next.insert("phase", "cancel_requested".into());
                    }
                }
            }
            "snapshot" => {
                require(
                    truthy(f(run, "primaryResourceRef"))
                        && truthy(f(run, "turnId"))
                        && truthy(f(run, "nativeEpoch")),
                    "task_board_turn_mismatch",
                    "A snapshot cannot infer a lost turn/start identity.",
                )?;
                let plan = self
                    .store
                    .read(
                        "task-board/native-plan",
                        f(f(run, "originalRequest"), "intent"),
                        false,
                    )
                    .await?;
                let observation = self
                    .evidence
                    .read(
                        run_pin,
                        f(change, "evidence"),
                        &EvidenceIdentity {
                            caller_principal_id: self.engine.principal_id().clone(),
                            service_node_id: s(f(run, "target"), "serviceNodeId")?.clone(),
                            native_version: s(&plan.value, "nativeVersion")?.clone(),
                            operation_id: s(change, "nativeOperationId")?.clone(),
                            method: "thread/read".into(),
                            params: Json::object([
                                (
                                    "threadId",
                                    f(f(run, "primaryResourceRef"), "nativeId").clone(),
                                ),
                                ("includeTurns", true.into()),
                            ]),
                        },
                    )
                    .await?;
                let reply = f(&observation, "reply");
                require(
                    eq(&observation, "phase", "succeeded") && truthy(reply) && has(reply, "result"),
                    "task_board_native_evidence_required",
                    "Native state publication requires a successful exact thread read.",
                )?;
                let thread = f(f(reply, "result"), "thread");
                require(
                    f(thread, "id") == f(f(run, "primaryResourceRef"), "nativeId")
                        && f(thread, "ephemeral") != &Json::Bool(true),
                    "task_board_primary_mismatch",
                    "The observed primary differs from the saved native context.",
                )?;
                let turn = array(f(thread, "turns"))
                    .iter()
                    .find(|v| f(v, "id") == f(run, "turnId"));
                require(
                    turn.is_some(),
                    "task_board_turn_unavailable",
                    "The owner snapshot does not contain the saved turn; do not infer completion.",
                )?;
                let turn = turn.unwrap();
                require(
                    n(change, "notificationCursor") >= n(run, "notificationCursor"),
                    "task_board_cursor_regression",
                    "Native notification progress cannot move backwards.",
                )?;
                next.insert(
                    "notificationCursor",
                    f(change, "notificationCursor").clone(),
                );
                require(
                    text(f(turn, "id")) == text(f(&next, "turnId")),
                    "task_board_turn_mismatch",
                    "A native snapshot cannot substitute another turn for the saved execution identity.",
                )?;
                let status = text(f(turn, "status"));
                if ["completed", "failed", "interrupted"]
                    .iter()
                    .any(|v| status == JsString::from(*v))
                {
                    done = completion(
                        if status == JsString::from("completed") {
                            "succeeded"
                        } else if status == JsString::from("interrupted")
                            && truthy(f(&next, "cancellation"))
                        {
                            "cancelled"
                        } else {
                            "failed"
                        },
                        Json::String(summary(turn, &status)),
                        f(change, "evidence"),
                        if status == JsString::from("completed") {
                            Json::Null
                        } else {
                            Json::String(join(&["native_".into(), status.clone()], ""))
                        },
                        Some(self.engine.retry_at(turn, s(op, "createdAt")?)?),
                    );
                } else {
                    require(
                        status == JsString::from("inProgress"),
                        "task_board_native_state_unsupported",
                        "The native turn has an unsupported state.",
                    )?;
                }
                let state = read_outcome(run, &status)?;
                if truthy(&done) {
                    done = patch(
                        &done,
                        [
                            ("state", f(&state, "state").clone()),
                            ("code", f(&state, "code").clone()),
                        ],
                    );
                } else {
                    let projected = project_native_state(
                        run,
                        &status,
                        f(&observation, "epoch"),
                        thread,
                        s(op, "createdAt")?,
                        f(change, "evidence"),
                    )?;
                    next = patch(
                        &next,
                        [
                            ("phase", f(&projected, "phase").clone()),
                            ("externalOutcome", f(&projected, "externalOutcome").clone()),
                        ],
                    );
                    waiting = f(&projected, "waiting").clone();
                }
                detail = "Observed the exact saved native turn.".into();
            }
            "unavailable" => {
                detail = f(change, "detail").clone();
                kind = "outcome_unknown";
                if eq(change, "code", "native_process_lost") {
                    require(
                        eq(change, "reason", "external_outcome")
                            && truthy(f(run, "turnId"))
                            && truthy(f(run, "nativeEpoch"))
                            && !one(
                                f(run, "externalOutcome"),
                                "state",
                                &["succeeded", "failed", "cancelled"],
                            ),
                        "task_board_outcome_unresolved",
                        "Only a previously unfinished dispatched turn can close after proven owner process loss.",
                    )?;
                    let proof = if f(f(run, "externalOutcome"), "evidence") == &Json::Null {
                        run_artifact(&original.document)
                    } else {
                        f(f(run, "externalOutcome"), "evidence").clone()
                    };
                    done = completion(
                        "failed",
                        detail.clone(),
                        &proof,
                        f(change, "code").clone(),
                        None,
                    );
                    kind = "result_saved";
                } else {
                    waiting = Json::object([
                        ("reason", f(change, "reason").clone()),
                        ("detail", detail.clone()),
                        ("since", at.clone()),
                    ]);
                    if one(
                        f(run, "externalOutcome"),
                        "state",
                        &["succeeded", "failed", "cancelled"],
                    ) {
                        next.insert("phase", "publishing_result".into());
                    } else if eq(change, "reason", "host")
                        && !truthy(f(f(run, "calls"), "thread"))
                        && !truthy(f(f(run, "calls"), "turn"))
                    {
                        next.insert(
                            "phase",
                            if truthy(f(run, "cancellation")) {
                                "cancel_requested".into()
                            } else {
                                "starting".into()
                            },
                        );
                    } else {
                        next = patch(
                            &next,
                            [
                                ("phase", "outcome_unknown".into()),
                                (
                                    "externalOutcome",
                                    patch(
                                        f(run, "externalOutcome"),
                                        [
                                            ("state", "unknown".into()),
                                            ("code", f(change, "code").clone()),
                                            ("observedAt", at.clone()),
                                        ],
                                    ),
                                ),
                            ],
                        );
                    }
                }
            }
            "cancelUnstarted" | "retireUnstarted" => {
                let cancelling = eq(change, "kind", "cancelUnstarted");
                require(
                    !truthy(f(f(run, "calls"), "turn"))
                        && !truthy(f(run, "turnId"))
                        && !truthy(f(f(run, "calls"), "interrupt"))
                        && if cancelling {
                            truthy(f(run, "cancellation"))
                        } else {
                            !active && !truthy(f(run, "cancellation"))
                        },
                    "task_board_outcome_unresolved",
                    "An execution cannot close as unstarted after turn preparation or without the required intent.",
                )?;
                let mut proof = run_artifact(&original.document);
                if truthy(f(f(run, "calls"), "thread")) {
                    let (c, o) = self.observed(run, f(f(run, "calls"), "thread")).await?;
                    require(
                        one(&o, "phase", &["succeeded", "failed"]),
                        "task_board_outcome_unresolved",
                        "Resolve the existing thread operation before closing an unstarted attempt.",
                    )?;
                    proof = f(&c.value, "evidence").clone();
                }
                done = completion(
                    if cancelling { "cancelled" } else { "failed" },
                    if cancelling {
                        "Cancelled before any native turn was prepared.".into()
                    } else {
                        "This superseded attempt closed before a native turn was prepared.".into()
                    },
                    &proof,
                    if cancelling {
                        "cancelled_before_turn".into()
                    } else {
                        "superseded_before_turn".into()
                    },
                    None,
                );
                detail = f(&done, "summary").clone();
            }
            _ => {}
        }
        if eq(change, "kind", "observeCall")
            && eq(change, "slot", "thread")
            && truthy(&done)
            && truthy(f(f(&next, "calls"), "turn"))
            && !truthy(f(&next, "turnId"))
        {
            let turn = self
                .store
                .read(
                    "task-board/native-call",
                    f(f(&next, "calls"), "turn"),
                    false,
                )
                .await?;
            if !eq(&turn.value, "ownerPhase", "failed") {
                let failed = self
                    .store
                    .read("task-board/native-call", f(change, "call"), false)
                    .await?;
                done = Json::Null;
                next = patch(
                    &next,
                    [
                        (
                            "phase",
                            if truthy(f(run, "cancellation")) {
                                "cancel_requested".into()
                            } else {
                                "outcome_unknown".into()
                            },
                        ),
                        (
                            "externalOutcome",
                            Json::object([
                                ("state", "unknown".into()),
                                ("observedAt", at.clone()),
                                ("evidence", f(&failed.value, "evidence").clone()),
                                ("code", "native_turn_resolution_required".into()),
                            ]),
                        ),
                    ],
                );
                waiting=wait(at,"external_outcome","The primary operation failed; the original prepared turn still requires owner reconciliation or prevention.".into());
            }
        }
        if eq(change, "kind", "observeCall")
            && truthy(f(run, "cancellation"))
            && !truthy(f(&next, "turnId"))
        {
            let mut prevented = Json::Null;
            let mut settled = true;
            for slot in ["thread", "turn"] {
                let pin = f(f(&next, "calls"), slot);
                if !truthy(pin) {
                    continue;
                }
                let call = self
                    .store
                    .read("task-board/native-call", pin, false)
                    .await?;
                if eq(&call.value, "state", "prepared") {
                    settled = false;
                    continue;
                }
                let (c, o) = self.observed(run, pin).await?;
                let fenced = eq(&o, "phase", "failed")
                    && eq(&o, "code", "request_prevented")
                    && f(&o, "requestId") == &Json::Null
                    && f(&o, "epoch") == &Json::Null
                    && f(&o, "reply") == &Json::Null;
                if fenced {
                    prevented = f(&c.value, "evidence").clone();
                }
                if if slot == "turn" {
                    !fenced
                } else {
                    !one(&o, "phase", &["succeeded", "failed"])
                } {
                    settled = false;
                }
            }
            if truthy(&prevented) {
                if settled {
                    done=completion("cancelled","Cancelled with the original native request durably prevented before dispatch.".into(),&prevented,"cancelled_before_dispatch".into(),None);
                } else {
                    done = Json::Null;
                    next = patch(
                        &next,
                        [
                            ("phase", "cancel_requested".into()),
                            (
                                "externalOutcome",
                                Json::object([
                                    ("state", "unknown".into()),
                                    ("observedAt", at.clone()),
                                    ("evidence", prevented),
                                    ("code", "native_prevention_pending".into()),
                                ]),
                            ),
                        ],
                    );
                    waiting=wait(at,"external_outcome","A request was prevented; remaining original calls still require owner reconciliation.".into());
                }
            }
        }
        if truthy(&done) && truthy(f(f(run, "calls"), "steer")) {
            let (_, o) = self.observed(run, f(f(run, "calls"), "steer")).await?;
            require(
                one(&o, "phase", &["succeeded", "failed"]),
                "task_board_outcome_unresolved",
                "Resolve the original steering receipt before closing its turn.",
            )?;
        }
        let publication = steps.pin()?;
        let mut final_task = patch(
            task_value,
            [
                ("updatedAt", at.clone()),
                ("publication", publication.clone()),
                (
                    "commentDeliveries",
                    Json::Array(
                        array(f(task_value, "commentDeliveries"))
                            .iter()
                            .map(|v| {
                                if steered.contains(f(v, "commentId")) {
                                    patch(
                                        v,
                                        [
                                            ("state", "delivered".into()),
                                            ("updatedAt", at.clone()),
                                            ("code", Json::Null),
                                        ],
                                    )
                                } else {
                                    v.clone()
                                }
                            })
                            .collect(),
                    ),
                ),
            ],
        );
        let coordination_only = eq(f(run, "originalRequest"), "action", "continue")
            && f(f(run, "originalRequest"), "coordinationOnly") == &Json::Bool(true);
        let reserved = steps
            .write(
                "task-board/task",
                &final_task,
                &Json::object([
                    ("objectId", Json::String(task_id.clone())),
                    ("expectedRevision", f(request, "expectedRevision").clone()),
                ]),
            )
            .await?;
        let mut out = outcome(f(request, "operationId"));
        if truthy(&done) {
            if let Ok(observed) = self.engine.workspace(task_value, &next).await {
                let workspace = Json::object(
                    observed
                        .as_object()
                        .unwrap_or(&[])
                        .iter()
                        .filter(|(k, _)| k != &JsString::from("nativeProjectId"))
                        .cloned(),
                );
                if f(&workspace, "hostId") == f(f(&next, "target"), "hostId")
                    && f(&workspace, "serviceNodeId") == f(f(&next, "target"), "serviceNodeId")
                    && f(&workspace, "intendedPath") == f(f(&next, "workspace"), "intendedPath")
                {
                    next.insert("workspace", workspace);
                }
            }
            let proof = f(&done, "evidence");
            let evidence = if truthy(proof)
                && self
                    .store
                    .local_key(s(f(proof, "object"), "objectId")?)?
                    .is_none()
            {
                vec![proof.clone()]
            } else {
                vec![]
            };
            let workspace = f(&next, "workspace");
            let repo = f(workspace, "repository");
            let repository = if truthy(f(workspace, "project")) && truthy(repo) {
                Json::object([
                    ("hostId", f(f(&next, "target"), "hostId").clone()),
                    (
                        "serviceNodeId",
                        f(f(&next, "target"), "serviceNodeId").clone(),
                    ),
                    ("project", f(workspace, "project").clone()),
                    ("repositoryName", f(repo, "name").clone()),
                    ("branch", f(repo, "branch").clone()),
                    ("commit", f(repo, "commit").clone()),
                    ("originName", f(repo, "originName").clone()),
                    ("originUrl", f(repo, "originUrl").clone()),
                    ("originState", f(repo, "originState").clone()),
                    ("remoteRef", f(repo, "remoteRef").clone()),
                    ("observedAt", f(repo, "observedAt").clone()),
                    ("limitation", f(repo, "limitation").clone()),
                ])
            } else {
                Json::Null
            };
            let content=Json::object([("summary",f(&done,"summary").clone()),("artifacts",Json::Array(evidence.clone())),("checks",Json::Array(vec![Json::object([("name","Native execution outcome".into()),("status",if eq(&done,"state","succeeded"){"passed".into()}else{"failed".into()}),("detail",if f(&done,"code")==&Json::Null{"Owner-confirmed native completion; acceptance criteria require user review.".into()}else{f(&done,"code").clone()}),("evidence",Json::Array(evidence))])])),("codeReferences",Json::Array(vec![])),("repositoryResult",repository),("limitations",Json::Array(vec!["Native completion does not automatically accept the task. Native conversation history remains available from its AgentManager owner while retained there.".into()]))]);
            let result = steps
                .create(
                    "task-board/result",
                    &Json::object([
                        ("schemaVersion", Json::Number(1.0)),
                        ("taskId", Json::String(task_id.clone())),
                        ("run", run_pin.clone()),
                        ("kind", "final".into()),
                        ("content", content),
                        ("createdAt", at.clone()),
                        ("actor", f(op, "actor").clone()),
                        ("operationId", f(request, "operationId").clone()),
                    ]),
                    &JsString::from("Native result"),
                )
                .await?;
            out.insert("result", result.clone());
            next = patch(
                &next,
                [
                    ("result", result),
                    (
                        "phase",
                        if eq(&done, "state", "succeeded") {
                            "completed".into()
                        } else if eq(&done, "state", "cancelled") {
                            "cancelled".into()
                        } else {
                            "failed".into()
                        },
                    ),
                    ("finishedAt", at.clone()),
                    (
                        "externalOutcome",
                        Json::object([
                            ("state", f(&done, "state").clone()),
                            ("observedAt", at.clone()),
                            ("evidence", proof.clone()),
                            ("code", f(&done, "code").clone()),
                        ]),
                    ),
                ],
            );
            kind = if eq(&done, "state", "cancelled") {
                "cancelled"
            } else {
                "result_saved"
            };
            detail = Json::String(join(
                &[
                    "Saved native ".into(),
                    text(f(&done, "state")),
                    if active {
                        " outcome.".into()
                    } else {
                        " outcome for the retained reassigned attempt.".into()
                    },
                ],
                "",
            ));
        }
        let mut retired: Vec<Json> = if finished_run(&next) {
            f(&next, "calls")
                .as_object()
                .unwrap_or(&[])
                .iter()
                .filter(|(_, v)| truthy(v))
                .map(|(_, v)| v.clone())
                .collect()
        } else {
            vec![]
        };
        let mut seen: HashSet<JsString> = retired
            .iter()
            .map(|v| s(v, "objectId").cloned())
            .collect::<IvyResult<_>>()?;
        let mut index = 0;
        while index < retired.len() {
            require(
                index < 2048,
                "task_board_evidence_mismatch",
                "The native call succession chain is too long.",
            )?;
            let call = self
                .store
                .read("task-board/native-call", &retired[index], false)
                .await?;
            let origin = f(&call.value, "origin");
            let prior = if one(origin, "kind", &["reattach", "steer"]) {
                f(origin, "predecessor")
            } else {
                &Json::Null
            };
            if truthy(prior) && seen.insert(s(prior, "objectId")?.clone()) {
                retired.push(prior.clone());
            }
            index += 1;
        }
        if finished_run(&next) {
            let proof = f(f(&next, "externalOutcome"), "evidence");
            let retained = if truthy(proof)
                && self
                    .store
                    .local_key(s(f(proof, "object"), "objectId")?)?
                    .is_none()
            {
                proof.clone()
            } else {
                Json::Null
            };
            next = patch(
                &next,
                [
                    (
                        "calls",
                        Json::object([
                            ("thread", Json::Null),
                            ("turn", Json::Null),
                            ("interrupt", Json::Null),
                        ]),
                    ),
                    (
                        "externalOutcome",
                        patch(f(&next, "externalOutcome"), [("evidence", retained)]),
                    ),
                ],
            );
        }
        out.insert(
            "run",
            steps
                .write(
                    "task-board/run",
                    &next,
                    &Json::object([
                        ("objectId", Json::String(run_id.clone())),
                        ("expectedRevision", f(run_pin, "revision").clone()),
                    ]),
                )
                .await?,
        );
        if active {
            if truthy(&done) {
                final_task = self
                    .engine
                    .finish_active(
                        &Json::object([
                            ("task", task_value.clone()),
                            ("run", run.clone()),
                            ("nextRun", next.clone()),
                            ("finalTask", final_task.clone()),
                            ("completion", done.clone()),
                            ("outcome", out.clone()),
                            ("at", at.clone()),
                            ("publication", publication.clone()),
                            ("coordinationOnly", coordination_only.into()),
                        ]),
                        steps,
                    )
                    .await?;
            } else {
                let state = if truthy(&waiting)
                    && !one(
                        &waiting,
                        "reason",
                        &["host", "external_outcome", "publication"],
                    ) {
                    "waiting"
                } else {
                    "in_progress"
                };
                let phase = if one(&waiting, "reason", &["host", "external_outcome"])
                    || eq(&next, "phase", "outcome_unknown")
                {
                    "outcome_unknown"
                } else if eq(&next, "phase", "cancel_requested") {
                    "cancel_requested"
                } else if eq(&next, "phase", "waiting_input") {
                    "waiting_input"
                } else if eq(&next, "phase", "publishing_result") {
                    "publishing_result"
                } else if eq(&next, "phase", "running") {
                    "running"
                } else {
                    "starting"
                };
                let delivered = if eq(&next, "phase", "running") {
                    Json::Array(
                        array(f(&final_task, "commentDeliveries"))
                            .iter()
                            .map(|v| {
                                if array(f(f(&next, "originalRequest"), "commentIds"))
                                    .contains(f(v, "commentId"))
                                {
                                    patch(
                                        v,
                                        [
                                            ("state", "delivered".into()),
                                            ("updatedAt", at.clone()),
                                            ("code", Json::Null),
                                        ],
                                    )
                                } else {
                                    v.clone()
                                }
                            })
                            .collect(),
                    )
                } else {
                    f(&final_task, "commentDeliveries").clone()
                };
                final_task = patch(
                    &final_task,
                    [
                        (
                            "claim",
                            patch(
                                f(task_value, "claim"),
                                [("phase", phase.into()), ("run", f(&out, "run").clone())],
                            ),
                        ),
                        ("primaryResourceRef", f(&next, "primaryResourceRef").clone()),
                        ("lastRun", f(&out, "run").clone()),
                        ("workflowState", state.into()),
                        (
                            "waiting",
                            if state == "waiting" {
                                waiting.clone()
                            } else {
                                Json::Null
                            },
                        ),
                        ("commentDeliveries", delivered),
                    ],
                );
            }
        } else if f(f(task_value, "lastRun"), "objectId") == f(run_pin, "objectId") {
            final_task.insert("lastRun", f(&out, "run").clone());
        }
        if truthy(&done) || waiting.get("reason") != f(task_value, "waiting").get("reason") {
            out.insert(
                "history",
                steps
                    .create(
                        "task-board/history",
                        &Json::object([
                            ("schemaVersion", Json::Number(1.0)),
                            ("taskId", Json::String(task_id.clone())),
                            ("operationId", f(request, "operationId").clone()),
                            ("actor", f(op, "actor").clone()),
                            ("createdAt", at.clone()),
                            ("kind", kind.into()),
                            ("detail", detail),
                            ("previousTask", task.document.pin.clone()),
                            ("previousHistory", f(task_value, "lastHistory").clone()),
                            ("priorRun", run_pin.clone()),
                            ("fromTarget", Json::Null),
                            ("toTarget", Json::Null),
                            ("result", f(&out, "result").clone()),
                            ("review", Json::Null),
                        ]),
                        &JsString::from("Native history"),
                    )
                    .await?,
            );
        }
        final_task = patch(
            &final_task,
            [
                ("publication", Json::Null),
                (
                    "lastHistory",
                    if f(&out, "history") == &Json::Null {
                        f(task_value, "lastHistory").clone()
                    } else {
                        f(&out, "history").clone()
                    },
                ),
            ],
        );
        out.insert(
            "task",
            steps
                .write(
                    "task-board/task",
                    &final_task,
                    &Json::object([
                        ("objectId", Json::String(task_id.clone())),
                        ("expectedRevision", f(&reserved, "revision").clone()),
                    ]),
                )
                .await?,
        );
        if active
            && truthy(&done)
            && (eq(&done, "state", "cancelled")
                || self.engine.shared_project(
                    f(f(task_value, "fields"), "workspaceRequirement"),
                    f(&next, "workspace"),
                )?)
        {
            self.store.release_workspace(task_id)?;
        }
        if finished_run(&next) {
            let published = self.engine.now();
            let operation_id = s(&operation.pin, "objectId")?;
            for root in std::iter::once(run_pin).chain(retired.iter()) {
                self.store.complete_after_publication(
                    s(root, "objectId")?,
                    &published,
                    operation_id,
                )?;
            }
            let intent = f(f(&next, "originalRequest"), "intent");
            if eq(f(&next, "owner"), "source", "scheduler")
                && self.store.local_key(s(intent, "objectId")?)?.as_deref()
                    == Some("task-board/native-plan")
            {
                self.store.complete_after_publication(
                    s(intent, "objectId")?,
                    &published,
                    operation_id,
                )?;
            }
        }
        Ok(out)
    }
}
