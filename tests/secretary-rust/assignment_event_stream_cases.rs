//! Stateful event protocol tests: issued pages, immutable subscriptions, CAS,
//! retained revisions and original mutation receipts survive a worker restart.
use super::*;
use ivy_contracts::{hash::hash_json, json::canonical};
use ivy_sdk::clock::{Clock, date_to_iso};
use ivy_secretary::{
    assignment_event_stream::event_filter,
    assignment_progress::SchedulerAssignments,
    assignment_scheduler::AssignmentScheduler,
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    store::{Record, Store, record_references},
};
use std::collections::BTreeMap;

const ASSIGNMENT: &str = "00000000-0000-4000-8000-000000000003";
const NOW: i64 = 1767235600000;
fn f<'a>(v: &'a IvyJson, k: &str) -> &'a IvyJson {
    static NULL: IvyJson = IvyJson::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s(v: &IvyJson, k: &str) -> String {
    f(v, k).as_str().unwrap()
}
fn seed() -> IvyJson {
    let bytes = reference_data::read(include_bytes!("assignment-events-progress-cases.json.gz"));
    let data = parse(std::str::from_utf8(&bytes).unwrap(), 32 * 1024 * 1024).unwrap();
    data.required("seed").unwrap().clone()
}
#[derive(Clone)]
struct Object {
    key: String,
    name: String,
    values: Vec<IvyJson>,
    times: Vec<IvyJson>,
    archived: bool,
}
struct Subscription {
    filter: IvyJson,
    initial: u64,
    ack: u64,
    issued: Option<IvyJson>,
}
struct State {
    root: String,
    settings: IvyJson,
    objects: BTreeMap<String, Object>,
    events: Vec<IvyJson>,
    pruned: u64,
    subscriptions: BTreeMap<String, Subscription>,
    receipts: BTreeMap<String, (IvyJson, IvyJson)>,
    fault: Option<(String, bool)>,
    trace: Vec<(String, IvyJson)>,
    returned_items: usize,
}
impl Default for State {
    fn default() -> Self {
        Self {
            root: String::new(),
            settings: IvyJson::Null,
            objects: BTreeMap::new(),
            events: Vec::new(),
            pruned: 0,
            subscriptions: BTreeMap::new(),
            receipts: BTreeMap::new(),
            fault: None,
            trace: Vec::new(),
            returned_items: 0,
        }
    }
}
impl State {
    fn new(seed: &IvyJson) -> Self {
        let raw = f(seed, "assignment");
        let mut out = Self {
            root: s(f(raw, "object"), "parentId"),
            settings: f(seed, "settings").clone(),
            ..Default::default()
        };
        out.objects.insert(
            ASSIGNMENT.into(),
            Object {
                key: "secretary/assignment".into(),
                name: ivy_secretary::assignment_schema::assignment_name(
                    f(f(f(raw, "content"), "value"), "assignmentId")
                        .as_js_string()
                        .unwrap(),
                )
                .unwrap(),
                values: vec![f(f(raw, "content"), "value").clone()],
                times: vec![f(f(raw, "revision"), "createdAt").clone()],
                archived: false,
            },
        );
        out
    }
    fn key(name: &str) -> Record {
        match name {
            "secretary/assignment" => Record::Assignment,
            "secretary/event-progress" => Record::EventProgress,
            "secretary/execution" => Record::Execution,
            _ => panic!("Unexpected record {name}"),
        }
    }
    fn read(&self, id: &str, revision: Option<usize>) -> IvyJson {
        let o = &self.objects[id];
        let rev = revision.unwrap_or(o.values.len());
        let v = &o.values[rev - 1];
        let (version, references) = if o.key == "fixture/object" {
            ("1.0.0", IvyJson::Object(vec![]))
        } else {
            let key = Self::key(&o.key);
            (
                *key.versions().last().unwrap(),
                record_references(key, v).unwrap(),
            )
        };
        IvyJson::object([
            (
                "object",
                IvyJson::object([
                    ("id", id.into()),
                    ("parentId", self.root.clone().into()),
                    ("name", o.name.clone().into()),
                    ("contractKey", o.key.clone().into()),
                    ("effectivelyArchived", o.archived.into()),
                ]),
            ),
            (
                "revision",
                IvyJson::object([
                    ("revision", (rev as f64).into()),
                    ("contractVersion", version.into()),
                    ("mediaType", "application/json".into()),
                    ("contentHash", hash_json(v).unwrap().into()),
                    (
                        "byteLength",
                        (canonical(v, usize::MAX).unwrap().len() as f64).into(),
                    ),
                    ("references", references),
                    ("createdAt", o.times[rev - 1].clone()),
                ]),
            ),
            (
                "content",
                IvyJson::object([("encoding", "json".into()), ("value", v.clone())]),
            ),
        ])
    }
    fn assignment(&self) -> IvyJson {
        let r = self.read(ASSIGNMENT, None);
        IvyJson::object([
            (
                "pin",
                IvyJson::object([
                    ("objectId", ASSIGNMENT.into()),
                    ("revision", f(f(&r, "revision"), "revision").clone()),
                ]),
            ),
            ("value", f(f(&r, "content"), "value").clone()),
            ("writtenAt", f(f(&r, "revision"), "createdAt").clone()),
        ])
    }
    fn push(&mut self, topic: &str, payload: IvyJson) -> u64 {
        let seq = self
            .events
            .last()
            .map(|e| f(e, "sequence").as_f64().unwrap() as u64)
            .unwrap_or(self.pruned)
            + 1;
        self.events.push(IvyJson::object([
            ("sequence", (seq as f64).into()),
            ("topic", topic.into()),
            ("topicVersion", "1.0.0".into()),
            ("source", "service:fixture.source".into()),
            ("mutationId", format!("mutation-{seq}").into()),
            (
                "occurredAt",
                IvyJson::String(date_to_iso((1767225600000 + seq * 1000) as f64).unwrap()),
            ),
            ("payload", payload),
        ]));
        seq
    }
    fn change(&mut self, topic: &str) -> u64 {
        let mut trigger = f(self.objects[ASSIGNMENT].values.last().unwrap(), "trigger").clone();
        trigger.insert("topic", topic.into());
        self.change_trigger(trigger)
    }
    fn change_trigger(&mut self, trigger: IvyJson) -> u64 {
        let object = self.objects.get_mut(ASSIGNMENT).unwrap();
        let mut value = object.values.last().unwrap().clone();
        value.insert("trigger", trigger);
        let revision = object.values.len() + 1;
        let seq = self.push(
            "hive.object.changed",
            IvyJson::object([
                ("objectId", ASSIGNMENT.into()),
                ("operation", "write".into()),
                ("revision", (revision as f64).into()),
            ]),
        );
        let time = f(self.events.last().unwrap(), "occurredAt").clone();
        value.insert("updatedAt", time.clone());
        let object = self.objects.get_mut(ASSIGNMENT).unwrap();
        object.values.push(value);
        object.times.push(time);
        seq
    }
    fn progress(&self) -> &IvyJson {
        self.objects
            .values()
            .find(|o| o.key == "secretary/event-progress")
            .unwrap()
            .values
            .last()
            .unwrap()
    }
    fn executions(&self) -> Vec<&IvyJson> {
        self.objects
            .values()
            .filter(|o| o.key == "secretary/execution")
            .map(|o| o.values.last().unwrap())
            .collect()
    }
    fn site(&self, method: &str, p: &IvyJson) -> String {
        if method == "objects.write" {
            let key = p
                .get("create")
                .map(|v| s(v, "contractKey"))
                .unwrap_or_else(|| self.objects[&s(p, "objectId")].key.clone());
            return format!("write:{key}");
        }
        method.into()
    }
    fn call(&mut self, method: &str, p: IvyJson) -> Result<IvyJson, IvyError> {
        self.trace.push((method.into(), p.clone()));
        let site = self.site(method, &p);
        let fault = self.fault.as_ref().filter(|(s, _)| s == &site).cloned();
        if fault.is_some() {
            self.fault = None;
        }
        if fault == Some((site.clone(), false)) {
            return Err(
                IvyError::new("injected", "Lost original reply.").with_outcome(Outcome::Unknown)
            );
        }
        let result = self.handle(method, &p)?;
        if fault == Some((site, true)) {
            return Err(
                IvyError::new("injected", "Lost original reply.").with_outcome(Outcome::Unknown)
            );
        }
        Ok(result)
    }
    fn handle(&mut self, method: &str, p: &IvyJson) -> Result<IvyJson, IvyError> {
        Ok(match method {
            "system.status" => IvyJson::object([("runtimeEpoch", "fixture".into())]),
            "serviceNodes.list" => IvyJson::object([
                ("items", IvyJson::Array(vec![])),
                ("nextCursor", IvyJson::Null),
            ]),
            "serviceNodes.get" => {
                let mut owner = f(&self.settings, "identity").clone();
                owner.insert("connected", true.into());
                owner.insert("synced", true.into());
                owner.insert("serviceName", "secretary".into());
                owner
            }
            "objects.query" => {
                let key = s(p, "contractKey");
                let name = f(f(p, "where"), "args")
                    .as_array()
                    .and_then(|args| args.iter().find(|x| f(x, "field") == &"object.name".into()))
                    .map(|v| s(v, "value"));
                let items = self
                    .objects
                    .iter()
                    .filter(|(_, o)| {
                        o.key == key && name.as_ref().is_none_or(|name| name == &o.name)
                    })
                    .map(|(id, o)| {
                        let mut row = IvyJson::object([
                            ("objectId", id.clone().into()),
                            ("revision", (o.values.len() as f64).into()),
                        ]);
                        if p.get("select").is_some() {
                            row.insert(
                                "values",
                                IvyJson::object([
                                    ("object.effectivelyArchived", o.archived.into()),
                                    ("object.archivedAt", IvyJson::Null),
                                    ("object.name", o.name.clone().into()),
                                    (
                                        "data:/assignmentId",
                                        f(o.values.last().unwrap(), "assignmentId").clone(),
                                    ),
                                ]),
                            );
                        }
                        row
                    })
                    .collect();
                IvyJson::object([
                    ("items", IvyJson::Array(items)),
                    ("nextCursor", IvyJson::Null),
                ])
            }
            "objects.read" => self.read(
                &s(p, "objectId"),
                p.get("revision")
                    .and_then(IvyJson::as_f64)
                    .map(|n| n as usize),
            ),
            "objects.write" => {
                let mutation = s(p, "mutationId");
                if let Some((original, result)) = self.receipts.get(&mutation) {
                    assert_eq!(original, p);
                    return Ok(result.clone());
                }
                let id = if let Some(create) = p.get("create") {
                    let name = s(create, "name");
                    assert!(!self.objects.values().any(|o| o.name == name));
                    let id = format!("object-{}", self.objects.len());
                    self.objects.insert(
                        id.clone(),
                        Object {
                            key: s(create, "contractKey"),
                            name,
                            values: vec![],
                            times: vec![],
                            archived: false,
                        },
                    );
                    id
                } else {
                    s(p, "objectId")
                };
                let object = self.objects.get_mut(&id).unwrap();
                if let Some(rev) = p.get("expectedRevision") {
                    if rev.as_f64() != Some(object.values.len() as f64) {
                        return Err(IvyError::new("revision_conflict", "Changed revision."));
                    }
                }
                object.values.push(f(f(p, "content"), "value").clone());
                object
                    .times
                    .push(IvyJson::String(date_to_iso(NOW as f64).unwrap()));
                let result = IvyJson::object([
                    ("object", IvyJson::object([("id", id.into())])),
                    (
                        "revision",
                        IvyJson::object([("revision", (object.values.len() as f64).into())]),
                    ),
                ]);
                self.receipts.insert(mutation, (p.clone(), result.clone()));
                result
            }
            "events.subscribe" => {
                let name = s(p, "name");
                let filter = f(p, "filter");
                let initial = f(p, "initialSequence").as_f64().unwrap() as u64;
                let sub = self
                    .subscriptions
                    .entry(name)
                    .or_insert_with(|| Subscription {
                        filter: filter.clone(),
                        initial,
                        ack: initial,
                        issued: None,
                    });
                assert_eq!(&sub.filter, filter);
                assert_eq!(sub.initial, initial);
                if let Some(value) = &sub.issued {
                    if !f(value, "gap").is_null()
                        || f(value, "items").as_array().unwrap().iter().all(|event| {
                            f(event, "sequence").as_f64().unwrap() as u64 > self.pruned
                        })
                    {
                        return Ok(value.clone());
                    }
                    sub.issued = None;
                }
                let head = self
                    .events
                    .last()
                    .map(|e| f(e, "sequence").as_f64().unwrap() as u64)
                    .unwrap_or(self.pruned);
                let mut through = sub.ack;
                let mut items = vec![];
                let mut gap = IvyJson::Null;
                if through < self.pruned {
                    through = self.pruned;
                    gap = IvyJson::object([
                        ("prunedThroughSequence", (through as f64).into()),
                        ("resumeAfterSequence", (through as f64).into()),
                    ]);
                } else {
                    for event in self
                        .events
                        .iter()
                        .filter(|e| f(e, "sequence").as_f64().unwrap() as u64 > sub.ack)
                        .take(100)
                    {
                        through = f(event, "sequence").as_f64().unwrap() as u64;
                        if matches_filter(filter, event) {
                            items.push(event.clone());
                        }
                    }
                }
                self.returned_items += items.len();
                let result = IvyJson::object([
                    ("items", IvyJson::Array(items)),
                    ("throughSequence", (through as f64).into()),
                    ("hasMore", (head > through).into()),
                    ("gap", gap),
                ]);
                sub.issued = Some(result.clone());
                assert!(self.subscriptions.len() <= 2);
                result
            }
            "events.ack" => {
                let sub = self.subscriptions.get_mut(&s(p, "name")).unwrap();
                let issued = sub.issued.take().unwrap();
                assert_eq!(f(&issued, "throughSequence"), f(p, "throughSequence"));
                if !f(&issued, "gap").is_null() {
                    assert_eq!(
                        f(f(&issued, "gap"), "prunedThroughSequence"),
                        f(p, "gapThroughSequence")
                    );
                }
                sub.ack = f(p, "throughSequence").as_f64().unwrap() as u64;
                IvyJson::Null
            }
            "events.unsubscribe" => {
                let mutation = s(p, "mutationId");
                if let Some((original, result)) = self.receipts.get(&mutation) {
                    assert_eq!(original, p);
                    return Ok(result.clone());
                }
                let name = s(p, "name");
                if self.subscriptions.contains_key(&name) && !self.objects[ASSIGNMENT].archived {
                    assert!(
                        self.subscriptions.keys().any(|other| other != &name),
                        "Replacement must be durable before retirement"
                    );
                }
                self.subscriptions.remove(&name);
                let result = IvyJson::object([("unsubscribed", true.into())]);
                self.receipts.insert(mutation, (p.clone(), result.clone()));
                result
            }
            other => panic!("Unexpected RPC {other}"),
        })
    }
}
fn matches_filter(filter: &IvyJson, event: &IvyJson) -> bool {
    for (field, event_field) in [("topics", "topic"), ("sources", "source")] {
        if let Some(values) = filter.get(field) {
            if !values.as_array().unwrap().contains(f(event, event_field)) {
                return false;
            }
        }
    }
    filter.get("objectIds").is_none_or(|ids| {
        f(event, "topic") == &"hive.object.changed".into()
            && ids
                .as_array()
                .unwrap()
                .contains(f(f(event, "payload"), "objectId"))
    })
}
struct Server(Mutex<State>);
impl RpcClient for Server {
    fn request<'a>(&'a self, method: &'a str, p: IvyJson, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move { self.0.lock().unwrap().call(method, p) })
    }
}
struct FixedClock;
impl Clock for FixedClock {
    fn utc_millis(&self) -> i64 {
        NOW
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
async fn tick(
    server: &Arc<Server>,
    journal: &Journal,
    seed: &IvyJson,
    assignment: &IvyJson,
    budget: usize,
    issues: &mut RecoveryScan,
) -> Result<bool, IvyError> {
    let root = f(f(f(seed, "assignment"), "object"), "parentId")
        .as_js_string()
        .unwrap()
        .clone();
    let store = Store {
        client: server.clone(),
        root,
        journal,
    };
    let service = "fixture.secretary".into();
    let evidence = InboxEvidence {
        store: &store,
        settings: f(seed, "settings"),
        service_node_id: &service,
        cancellation: RequestOptions::default(),
    };
    let scheduler = AssignmentScheduler {
        evidence: &evidence,
        tools: Arc::new(ToolClients::new(server.clone())),
        clock: &FixedClock,
        fallback_execution: None,
    };
    scheduler
        .events(assignment, f(seed, "configuration"), budget, issues)
        .await
}
fn issues() -> RecoveryScan {
    RecoveryScan {
        cursor: None,
        issues: vec![],
    }
}
async fn ready(seed: &IvyJson) -> (Arc<Server>, Journal, IvyJson) {
    let mut state = State::new(seed);
    state.push(
        "hive.object.changed",
        IvyJson::object([
            ("objectId", ASSIGNMENT.into()),
            ("operation", "write".into()),
            ("revision", 1.0.into()),
        ]),
    );
    let assignment = state.assignment();
    let server = Arc::new(Server(Mutex::new(state)));
    let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
    assert!(
        tick(&server, &journal, seed, &assignment, 8, &mut issues())
            .await
            .unwrap()
    );
    assert_eq!(server.0.lock().unwrap().subscriptions.len(), 1);
    (server, journal, assignment)
}
#[tokio::test]
async fn topic_changes_inside_issued_pages_keep_each_revision_and_replay_without_duplicates() {
    let seed = seed();
    let (server, journal, assignment) = ready(&seed).await;
    let payload = parse(
        r#"{"text":"Original\ud800","unknown":{"rows":[1,null,true]}}"#,
        4096,
    )
    .unwrap();
    {
        let mut state = server.0.lock().unwrap();
        state.change("second.topic");
        state.push("second.topic", payload.clone());
        state.change("third.topic");
        state.push("third.topic", IvyJson::Null);
        state.push("fixture.events", IvyJson::Null);
    }
    assert!(
        tick(&server, &journal, &seed, &assignment, 12, &mut issues())
            .await
            .unwrap()
    );
    {
        let mut state = server.0.lock().unwrap();
        let runs = state.executions();
        assert_eq!(runs.len(), 2);
        assert!(
            runs.iter()
                .any(|run| f(f(f(run, "trigger"), "payload"), "data") == &payload)
        );
        assert!(
            runs.iter()
                .any(|r| f(f(r, "assignment"), "revision") == &2.0.into()
                    && f(f(r, "trigger"), "key") == &"event:3:mutation-3".into())
        );
        assert!(
            runs.iter()
                .any(|r| f(f(r, "assignment"), "revision") == &3.0.into()
                    && f(f(r, "trigger"), "key") == &"event:5:mutation-5".into())
        );
        state.subscriptions.clear();
    }
    assert!(
        tick(&server, &journal, &seed, &assignment, 12, &mut issues())
            .await
            .unwrap()
    );
    assert_eq!(server.0.lock().unwrap().executions().len(), 2);
}
#[tokio::test]
async fn gap_rebases_from_the_first_global_event_before_selecting_a_new_topic() {
    for existing_issues in [0, 31] {
        let seed = seed();
        let (server, journal, assignment) = ready(&seed).await;
        {
            let mut state = server.0.lock().unwrap();
            state.change("new.topic");
            for _ in 0..3 {
                state.push("irrelevant", IvyJson::Null);
            }
            state.pruned = 5;
            state.events.clear();
            state.push("new.topic", IvyJson::Null);
        }
        let mut issues = issues();
        for i in 0..existing_issues {
            issues.issue(&format!("original-{i}"), "fixture");
        }
        assert!(
            tick(&server, &journal, &seed, &assignment, 12, &mut issues)
                .await
                .unwrap()
        );
        let state = server.0.lock().unwrap();
        assert_eq!(state.executions().len(), 1);
        assert_eq!(
            f(f(state.executions()[0], "assignment"), "revision"),
            &2.0.into()
        );
        if existing_issues == 0 {
            assert_eq!(issues.issues.len(), 1);
            assert_eq!(issues.issues[0].1, "secretary_event_gap");
        } else {
            assert_eq!(issues.issues.len(), 32);
            assert_eq!(
                issues.issues[31],
                (
                    "recovery-overflow".into(),
                    "secretary_recovery_issues_overflow".into()
                )
            );
        }
        assert_eq!(f(state.progress(), "needsRebase"), &false.into());
    }
}
#[tokio::test]
async fn lost_replies_before_and_after_effects_resume_original_streams_and_executions() {
    let seed = seed();
    for site in [
        "events.subscribe",
        "events.ack",
        "objects.read",
        "write:secretary/execution",
        "write:secretary/event-progress",
        "events.unsubscribe",
    ] {
        for after in [false, true] {
            let (server, journal, assignment) = ready(&seed).await;
            {
                let mut state = server.0.lock().unwrap();
                let topic = if site == "events.unsubscribe" {
                    "changed.topic"
                } else {
                    "fixture.events"
                };
                state.change(topic);
                state.push(topic, IvyJson::Null);
                state.fault = Some((site.into(), after));
            }
            let first = tick(&server, &journal, &seed, &assignment, 8, &mut issues()).await;
            assert!(
                server.0.lock().unwrap().fault.is_none(),
                "Fault not exercised: {site} {after}"
            );
            if let Err(error) = first {
                assert_eq!(error.code, "injected", "{site} {after}");
            }
            // A fresh journal/worker shares only durable Hive records and receipts.
            let restarted =
                Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
            assert!(
                tick(&server, &restarted, &seed, &assignment, 12, &mut issues())
                    .await
                    .unwrap(),
                "{site} {after}"
            );
            let state = server.0.lock().unwrap();
            assert_eq!(state.executions().len(), 1, "{site} {after}");
            assert_eq!(state.subscriptions.len(), 1);
            assert!(state.progress().get("retiringSubscription").is_none());
            let mut retired = BTreeMap::new();
            for (method, params) in &state.trace {
                if method == "events.unsubscribe" {
                    let name = s(params, "name");
                    let id = s(params, "mutationId");
                    assert!(
                        retired
                            .insert(name, id.clone())
                            .is_none_or(|previous| previous == id)
                    );
                }
            }
        }
    }
}
#[tokio::test]
async fn filtered_pages_skip_large_unrelated_payloads_and_foreign_progress_has_no_stream_effect() {
    let seed = seed();
    let (server, journal, assignment) = ready(&seed).await;
    let progress_revisions = server
        .0
        .lock()
        .unwrap()
        .objects
        .values()
        .find(|o| o.key == "secretary/event-progress")
        .unwrap()
        .values
        .len();
    {
        let mut state = server.0.lock().unwrap();
        state.returned_items = 0;
        for _ in 0..1000 {
            state.push(
                "other.topic",
                IvyJson::object([("text", "x".repeat(4096).into())]),
            );
        }
        state.push("fixture.events", IvyJson::Null);
    }
    assert!(
        tick(&server, &journal, &seed, &assignment, 16, &mut issues())
            .await
            .unwrap()
    );
    {
        let mut state = server.0.lock().unwrap();
        assert_eq!(state.returned_items, 1);
        assert_eq!(state.executions().len(), 1);
        assert_eq!(
            state
                .objects
                .values()
                .find(|o| o.key == "secretary/event-progress")
                .unwrap()
                .values
                .len(),
            progress_revisions
        );
        let progress = state
            .objects
            .values_mut()
            .find(|o| o.key == "secretary/event-progress")
            .unwrap();
        progress
            .values
            .last_mut()
            .unwrap()
            .insert("assignmentId", "another-assignment".into());
        state.trace.clear();
    }
    let error = tick(&server, &journal, &seed, &assignment, 16, &mut issues())
        .await
        .unwrap_err();
    assert_eq!(error.code, "secretary_event_progress_mismatch");
    assert!(
        !server
            .0
            .lock()
            .unwrap()
            .trace
            .iter()
            .any(|(m, _)| m.starts_with("events."))
    );
}

#[tokio::test]
async fn malformed_assignment_revisions_never_advance_or_acknowledge_the_page() {
    let seed = seed();
    for revision in [
        IvyJson::Null,
        0.0.into(),
        (-1.0).into(),
        1.5.into(),
        9007199254740992.0.into(),
        "2".into(),
        false.into(),
        true.into(),
        IvyJson::Array(vec![]),
    ] {
        let (server, journal, assignment) = ready(&seed).await;
        {
            let mut state = server.0.lock().unwrap();
            state.trace.clear();
            state.push(
                "hive.object.changed",
                IvyJson::object([
                    ("objectId", ASSIGNMENT.into()),
                    ("operation", "write".into()),
                    ("revision", revision),
                ]),
            );
        }
        let error = tick(&server, &journal, &seed, &assignment, 8, &mut issues())
            .await
            .unwrap_err();
        assert_eq!(error.code, "secretary_object_event_invalid");
        let state = server.0.lock().unwrap();
        assert!(state.executions().is_empty());
        assert!(!state.trace.iter().any(|(method, _)| method == "events.ack"));
    }
}

#[test]
fn hive_filters_preserve_admitted_matches_and_assignment_changes_from_any_writer() {
    let seed = seed();
    let data = parse(
        std::str::from_utf8(&reference_data::read(include_bytes!(
            "assignment-events-progress-cases.json.gz"
        )))
        .unwrap(),
        32 * 1024 * 1024,
    )
    .unwrap();
    let mut checked = 0;
    for row in f(&data, "matches")
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| f(r, "expected") == &true.into())
    {
        let mut assignment = f(f(f(&seed, "assignment"), "content"), "value").clone();
        let original = f(f(row, "assignment"), "trigger");
        let mut trigger = if f(original, "kind") == &"object-change".into() {
            IvyJson::object([
                ("kind", "object-change".into()),
                ("objectIds", IvyJson::Array(vec![])),
                ("paths", IvyJson::Array(vec![])),
                ("delaySeconds", 0.0.into()),
                ("observation", IvyJson::Null),
            ])
        } else {
            f(&assignment, "trigger").clone()
        };
        for (key, value) in original.as_object().unwrap() {
            trigger.insert(key.clone(), value.clone());
        }
        assignment.insert("trigger", trigger);
        assignment.insert("enabled", true.into());
        if ivy_secretary::schema::validate(ivy_secretary::schema::Schema::Assignment, &assignment)
            .is_err()
        {
            continue;
        }
        let progress = IvyJson::object([
            (
                "assignment",
                IvyJson::object([("objectId", ASSIGNMENT.into())]),
            ),
            ("assignmentSnapshot", assignment),
            ("active", true.into()),
            ("needsRebase", false.into()),
        ]);
        let filter = event_filter(&progress).unwrap();
        assert!(matches_filter(&filter, f(row, "event")));
        assert!(filter.get("sources").is_none());
        for source in [
            "principal:secretary-principal",
            "principal:another-authorized-writer",
        ] {
            let event = IvyJson::object([
                ("topic", "hive.object.changed".into()),
                ("source", source.into()),
                (
                    "payload",
                    IvyJson::object([("objectId", ASSIGNMENT.into())]),
                ),
            ]);
            assert!(matches_filter(&filter, &event));
        }
        checked += 1;
    }
    assert!(checked >= 10);
}

#[tokio::test]
async fn archive_retires_the_saved_current_and_original_pending_subscription() {
    let seed = seed();
    let (server, journal, assignment) = ready(&seed).await;
    {
        let mut state = server.0.lock().unwrap();
        state.change("new.topic");
        state.push("new.topic", IvyJson::Null);
    }
    assert!(
        !tick(&server, &journal, &seed, &assignment, 1, &mut issues())
            .await
            .unwrap()
    );
    let original = {
        let mut state = server.0.lock().unwrap();
        let original = f(state.progress(), "retiringSubscription").clone();
        state.objects.get_mut(ASSIGNMENT).unwrap().archived = true;
        original
    };
    let root = f(f(f(&seed, "assignment"), "object"), "parentId")
        .as_js_string()
        .unwrap()
        .clone();
    let store = Store {
        client: server.clone(),
        root,
        journal: &journal,
    };
    let service = "fixture.secretary".into();
    let evidence = InboxEvidence {
        store: &store,
        settings: f(&seed, "settings"),
        service_node_id: &service,
        cancellation: RequestOptions::default(),
    };
    let scheduler = AssignmentScheduler {
        evidence: &evidence,
        tools: Arc::new(ToolClients::new(server.clone())),
        clock: &FixedClock,
        fallback_execution: None,
    };
    let mut scan = SchedulerAssignments::default();
    assert!(
        scan.assignments(&scheduler, &mut issues())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(server.0.lock().unwrap().subscriptions.is_empty());
    let count = server
        .0
        .lock()
        .unwrap()
        .trace
        .iter()
        .filter(|(method, _)| method == "events.unsubscribe")
        .count();
    assert!(
        scan.assignments(&scheduler, &mut issues())
            .await
            .unwrap()
            .is_empty()
    );
    let state = server.0.lock().unwrap();
    assert_eq!(
        state
            .trace
            .iter()
            .filter(|(method, _)| method == "events.unsubscribe")
            .count(),
        count
    );
    assert!(
        state
            .trace
            .iter()
            .any(|(method, p)| method == "events.unsubscribe" && p == &original)
    );
}

#[tokio::test]
async fn a_gap_before_assignment_creation_does_not_claim_lost_assignment_work() {
    let seed = seed();
    for prior in [
        None,
        Some(IvyJson::Null),
        Some(false.into()),
        Some(true.into()),
    ] {
        let mut state = State::new(&seed);
        state.pruned = 50;
        state.push("other.topic", IvyJson::Null);
        state
            .events
            .last_mut()
            .unwrap()
            .insert("occurredAt", "2025-12-31T23:59:00.000Z".into());
        state.push(
            "hive.object.changed",
            IvyJson::object([
                ("objectId", ASSIGNMENT.into()),
                ("operation", "write".into()),
                ("revision", 1.0.into()),
            ]),
        );
        state.push("fixture.events", IvyJson::Null);
        let assignment = state.assignment();
        let server = Arc::new(Server(Mutex::new(state)));
        let journal = Journal::open(std::path::Path::new(":memory:"), Limits::default()).unwrap();
        if let Some(value) = &prior {
            journal
                .create_technical(
                    "secretary/event-gap",
                    "event-fixture",
                    &IvyJson::object([
                        ("prunedThroughSequence", 0.0.into()),
                        ("relevant", value.clone()),
                    ]),
                )
                .unwrap();
        }
        let mut issues = issues();
        assert!(
            tick(&server, &journal, &seed, &assignment, 16, &mut issues)
                .await
                .unwrap()
        );
        assert_eq!(
            issues.issues.is_empty(),
            prior.as_ref() != Some(&true.into())
        );
        assert_eq!(server.0.lock().unwrap().executions().len(), 1);
    }
}

#[tokio::test]
async fn foreign_assignment_and_retirement_records_cannot_delete_another_stream() {
    let seed = seed();
    let (server, journal, assignment) = ready(&seed).await;
    let original = server.0.lock().unwrap().progress().clone();
    for retiring in [false, true] {
        {
            let mut state = server.0.lock().unwrap();
            let record = state
                .objects
                .values_mut()
                .find(|o| o.key == "secretary/event-progress")
                .unwrap();
            let mut value = original.clone();
            if retiring {
                value.insert(
                    "retiringSubscription",
                    IvyJson::object([
                        ("name", "secretary-assignment-foreign".into()),
                        ("mutationId", "fixture:1767235600000:foreign".into()),
                    ]),
                );
            } else {
                value
                    .get_mut("assignment")
                    .unwrap()
                    .insert("objectId", "another-assignment-object".into());
            }
            *record.values.last_mut().unwrap() = value;
            state.trace.clear();
        }
        let error = tick(&server, &journal, &seed, &assignment, 8, &mut issues())
            .await
            .unwrap_err();
        assert_eq!(error.code, "secretary_event_progress_mismatch");
        assert!(
            !server
                .0
                .lock()
                .unwrap()
                .trace
                .iter()
                .any(|(method, _)| method == "events.unsubscribe")
        );
    }
}

#[tokio::test]
async fn restore_behind_the_effect_fence_advances_only_confirmed_pages() {
    let seed = seed();
    let (server, journal, assignment) = ready(&seed).await;
    {
        let mut state = server.0.lock().unwrap();
        for _ in 0..149 {
            state.push("other.topic", IvyJson::Null);
        }
        state.push("fixture.events", IvyJson::Null);
        for _ in 0..50 {
            state.push("other.topic", IvyJson::Null);
        }
        let progress = state
            .objects
            .values_mut()
            .find(|o| o.key == "secretary/event-progress")
            .unwrap();
        progress
            .values
            .last_mut()
            .unwrap()
            .insert("processedThrough", 150.0.into());
    }
    assert!(
        tick(&server, &journal, &seed, &assignment, 16, &mut issues())
            .await
            .unwrap()
    );
    let state = server.0.lock().unwrap();
    assert_eq!(state.executions().len(), 1);
    assert_eq!(
        f(f(state.executions()[0], "trigger"), "key"),
        &"event:151:mutation-151".into()
    );
}

#[tokio::test]
async fn missing_execution_configuration_leaves_the_original_page_unacknowledged() {
    let mut seed = seed();
    let (server, journal, assignment) = ready(&seed).await;
    let configured = f(f(&seed, "configuration"), "execution").clone();
    seed.get_mut("configuration")
        .unwrap()
        .insert("execution", IvyJson::Null);
    server
        .0
        .lock()
        .unwrap()
        .push("fixture.events", IvyJson::Null);
    let failure = tick(&server, &journal, &seed, &assignment, 8, &mut issues())
        .await
        .unwrap_err();
    assert_eq!(failure.code, "secretary_execution_unconfigured");
    assert!(
        server
            .0
            .lock()
            .unwrap()
            .subscriptions
            .values()
            .any(|s| s.issued.is_some())
    );
    seed.get_mut("configuration")
        .unwrap()
        .insert("execution", configured);
    assert!(
        tick(&server, &journal, &seed, &assignment, 8, &mut issues())
            .await
            .unwrap()
    );
    assert_eq!(server.0.lock().unwrap().executions().len(), 1);
}

#[tokio::test]
async fn disabling_and_enabling_within_one_page_uses_each_effective_revision() {
    let seed = seed();
    let (server, journal, assignment) = ready(&seed).await;
    {
        let mut state = server.0.lock().unwrap();
        state.change("fixture.events");
        state
            .objects
            .get_mut(ASSIGNMENT)
            .unwrap()
            .values
            .last_mut()
            .unwrap()
            .insert("enabled", false.into());
        state.push("fixture.events", IvyJson::Null);
        state.change("fixture.events");
        state
            .objects
            .get_mut(ASSIGNMENT)
            .unwrap()
            .values
            .last_mut()
            .unwrap()
            .insert("enabled", true.into());
        state.push("fixture.events", IvyJson::Null);
    }
    assert!(
        tick(&server, &journal, &seed, &assignment, 16, &mut issues())
            .await
            .unwrap()
    );
    let state = server.0.lock().unwrap();
    assert_eq!(state.executions().len(), 1);
    assert_eq!(
        f(f(state.executions()[0], "trigger"), "key"),
        &"event:5:mutation-5".into()
    );
    assert_eq!(
        f(f(state.executions()[0], "assignment"), "revision"),
        &3.0.into()
    );
}

#[tokio::test]
async fn changed_watched_objects_are_replayed_before_the_old_page_end() {
    let mut seed = seed();
    let trigger = IvyJson::object([
        ("kind", "object-change".into()),
        ("objectIds", IvyJson::Array(vec!["watched-A".into()])),
        ("paths", IvyJson::Array(vec!["/title".into()])),
        ("delaySeconds", 0.0.into()),
        ("observation", IvyJson::Null),
    ]);
    seed.get_mut("assignment")
        .unwrap()
        .get_mut("content")
        .unwrap()
        .get_mut("value")
        .unwrap()
        .insert("trigger", trigger.clone());
    let (server, journal, assignment) = ready(&seed).await;
    {
        let mut state = server.0.lock().unwrap();
        for id in ["watched-A", "watched-B"] {
            state.objects.insert(
                id.into(),
                Object {
                    key: "fixture/object".into(),
                    name: id.into(),
                    values: vec![
                        IvyJson::object([("title", "before".into())]),
                        IvyJson::object([("title", id.into())]),
                    ],
                    times: vec!["2026-01-01T00:00:00.000Z".into(); 2],
                    archived: false,
                },
            );
        }
        let mut next = trigger;
        next.insert("objectIds", IvyJson::Array(vec!["watched-B".into()]));
        state.change_trigger(next);
        for id in ["watched-B", "watched-A"] {
            state.push(
                "hive.object.changed",
                IvyJson::object([
                    ("objectId", id.into()),
                    ("revision", 2.0.into()),
                    ("operation", "write".into()),
                ]),
            );
        }
    }
    assert!(
        tick(&server, &journal, &seed, &assignment, 16, &mut issues())
            .await
            .unwrap()
    );
    let windows = journal
        .list(ivy_secretary::assignment_object::OBJECT_WINDOW_KEY, true)
        .unwrap();
    assert_eq!(windows.len(), 1);
    assert_eq!(f(&windows[0].value, "objectId"), &"watched-B".into());
    assert_eq!(
        f(f(f(&windows[0].value, "pending"), "assignment"), "revision"),
        &2.0.into()
    );
}

#[tokio::test]
async fn pruning_an_issued_page_after_a_lost_reply_still_reports_the_gap_and_rebases() {
    let seed = seed();
    let (server, journal, assignment) = ready(&seed).await;
    {
        let mut state = server.0.lock().unwrap();
        state.push("fixture.events", IvyJson::Null);
        state.fault = Some(("events.subscribe".into(), true));
    }
    assert_eq!(
        tick(&server, &journal, &seed, &assignment, 8, &mut issues())
            .await
            .unwrap_err()
            .code,
        "injected"
    );
    {
        let mut state = server.0.lock().unwrap();
        state.change("new.topic");
        state.push("irrelevant", IvyJson::Null);
        state.push("irrelevant", IvyJson::Null);
        state.pruned = 5;
        state.events.clear();
        state.push("new.topic", IvyJson::Null);
    }
    let mut issues = issues();
    assert!(
        tick(&server, &journal, &seed, &assignment, 16, &mut issues)
            .await
            .unwrap()
    );
    let state = server.0.lock().unwrap();
    assert_eq!(issues.issues.len(), 1);
    assert_eq!(state.executions().len(), 1);
    assert_eq!(
        f(f(state.executions()[0], "trigger"), "key"),
        &"event:6:mutation-6".into()
    );
    assert_eq!(
        f(f(state.executions()[0], "assignment"), "revision"),
        &2.0.into()
    );
}
