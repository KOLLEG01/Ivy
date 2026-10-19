use super::snapshot;
use ivy_chat_bridge::whatsapp::{
    journal::{HistoryLimits, OutputState},
    journal_io::JournalIo,
    journal_owner::Journal,
    listener::{
        ListenerMain, ListenerNative, WhatsAppListener, delegation_source, terminal_text,
        whatsapp_text,
    },
    messages::Translator,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    hash::hash_json,
    json::{canonical, parse, stringify, try_js_string},
};
use ivy_sdk::{clock::Clock, rpc::RequestFuture};
use std::{
    collections::HashMap,
    process::Command,
    sync::{Arc, Mutex},
    time::Instant,
};
fn f<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn s(value: &Json, key: &str) -> String {
    f(value, key).as_str().unwrap()
}
fn result(value: IvyResult<Json>) -> Json {
    match value {
        Ok(v) => Json::object([("value", v)]),
        Err(e) => Json::object([(
            "error",
            Json::object([
                ("code", Json::String(e.code_js())),
                ("message", Json::String(e.message_js())),
                ("outcome", e.outcome.as_str().into()),
            ]),
        )]),
    }
}
fn unwrap(value: &Json) -> IvyResult<Json> {
    if let Some(e) = value.get("throw") {
        return Err(IvyError::new(s(e, "code"), s(e, "message")).with_outcome(
            match s(e, "outcome").as_str() {
                "unknown" => Outcome::Unknown,
                "completed" => Outcome::Completed,
                _ => Outcome::NotExecuted,
            },
        ));
    }
    Ok(value.clone())
}
struct State {
    current: Json,
    now: i64,
    events: Vec<Json>,
    counts: HashMap<&'static str, usize>,
}
struct Probe {
    state: Mutex<State>,
    start: Instant,
}
impl Clock for Probe {
    fn utc_millis(&self) -> i64 {
        self.state.lock().unwrap().now
    }
    fn monotonic(&self) -> Instant {
        self.start
    }
}
impl Probe {
    fn event(&self, values: Vec<Json>) {
        self.state.lock().unwrap().events.push(Json::Array(values));
    }
    fn get(&self, key: &str, default: Json) -> IvyResult<Json> {
        let state = self.state.lock().unwrap();
        unwrap(state.current.get(key).unwrap_or(&default))
    }
    fn take(&self, key: &'static str, default: Json) -> IvyResult<Json> {
        let mut state = self.state.lock().unwrap();
        let at = state.counts.entry(key).or_default();
        let index = *at;
        *at += 1;
        let values = state
            .current
            .get(key)
            .unwrap_or(&default)
            .as_array()
            .unwrap();
        unwrap(&values[index.min(values.len() - 1)])
    }
    fn patch(&self, value: &Json) {
        let mut state = self.state.lock().unwrap();
        for (key, value) in value.as_object().unwrap() {
            state.current.insert(key.clone(), value.clone());
        }
        state.counts.clear();
    }
    fn collect(&self) -> Vec<Json> {
        std::mem::take(&mut self.state.lock().unwrap().events)
    }
}
fn base_binding() -> Json {
    parse(
        r#"{"pin":{"objectId":"binding","revision":1},"value":{"primary":{"nativeId":"thread"}}}"#,
        usize::MAX,
    )
    .unwrap()
}
fn agent(id: &str, body: &str) -> Json {
    Json::object([
        ("id", id.into()),
        ("type", "agentMessage".into()),
        ("text", body.into()),
    ])
}
fn turn(id: &str, items: Vec<Json>) -> Json {
    Json::object([
        ("id", id.into()),
        ("status", "completed".into()),
        ("itemsView", "full".into()),
        ("items", Json::Array(items)),
    ])
}
fn page(turns: Vec<Json>, next: Json) -> Json {
    Json::object([("data", Json::Array(turns)), ("nextCursor", next)])
}
fn default_page() -> Json {
    page(
        vec![turn(
            "turn",
            vec![
                Json::object([
                    ("id", "user".into()),
                    ("type", "userMessage".into()),
                    ("content", Json::Array(vec![])),
                ]),
                agent("answer", "Answer"),
            ],
        )],
        Json::Null,
    )
}
fn default_reply() -> Json {
    parse(r#"{"object":{"objectId":"reply","revision":1},"data":{"origin":{"kind":"notice"},"text":"Notice","channel":{"adapter":"whatsapp","accountId":"account","channelId":"channel"}}}"#,usize::MAX).unwrap()
}
fn received() -> Json {
    Json::object([("phase","succeeded".into()),("outcome",Json::object([
    ("action","receive".into()),("offer",parse(r#"{"object":{"objectId":"offer","revision":1},"data":{"throughSequence":3}}"#,usize::MAX).unwrap()),("replies",Json::Array(vec![default_reply()])),
]))])
}
impl ListenerNative for Probe {
    fn bound(&self) -> RequestFuture<'_> {
        Box::pin(async {
            self.event(vec!["bound".into()]);
            Ok(Json::object([(
                "binding",
                self.get("binding", base_binding())?,
            )]))
        })
    }
    fn turn_page<'a>(&'a self, thread: &'a JsString, cursor: Json) -> RequestFuture<'a> {
        Box::pin(async move {
            self.event(vec![
                "turnPage".into(),
                Json::String(thread.clone()),
                cursor.clone(),
            ]);
            let (wide, tool, paged) = {
                let mut state = self.state.lock().unwrap();
                state.now += f(&state.current, "pageAdvance").as_f64().unwrap_or(0.0) as i64;
                (
                    f(&state.current, "wideHistory").as_bool().unwrap_or(false),
                    f(&state.current, "wideTool").as_bool().unwrap_or(false),
                    f(&state.current, "pagedTurns").as_f64().unwrap_or(0.0) as usize,
                )
            };
            if wide {
                let id = if cursor.is_null() {
                    "0".into()
                } else {
                    try_js_string(&cursor)?.to_utf8()?
                };
                return Ok(page(
                    vec![turn(
                        &format!("wide-{id}"),
                        vec![agent("answer", &"x".repeat(17 * 1024 * 1024))],
                    )],
                    "next".into(),
                ));
            }
            if tool {
                let mut items = default_page().required("data")?.as_array().unwrap()[0]
                    .required("items")?
                    .as_array()
                    .unwrap()
                    .to_vec();
                items.push(Json::object([
                    ("id", "tool".into()),
                    ("type", "commandExecution".into()),
                    ("output", "x".repeat(2 * 1024 * 1024).into()),
                ]));
                return Ok(page(vec![turn("tool", items)], Json::Null));
            }
            if paged > 0 {
                let offset = if cursor.is_null() {
                    0
                } else {
                    cursor.as_str()?.parse::<usize>().unwrap()
                };
                let id = paged - 1 - offset;
                return Ok(page(
                    vec![turn(
                        &format!("turn-{id}"),
                        vec![agent(&format!("item-{id}"), &format!("Answer {id}"))],
                    )],
                    if offset + 1 < paged {
                        (offset + 1).to_string().into()
                    } else {
                        Json::Null
                    },
                ));
            }
            self.take("pages", Json::Array(vec![default_page()]))
        })
    }
    fn source_name(&self, thread: JsString) -> RequestFuture<'_> {
        Box::pin(async move {
            self.event(vec![
                "sourceName".into(),
                "thread/read".into(),
                Json::object([
                    ("threadId", Json::String(thread)),
                    ("includeTurns", false.into()),
                ]),
                true.into(),
            ]);
            self.get(
                "nameReply",
                parse(
                    r#"{"reply":{"result":{"thread":{"name":"Resolved source"}}}}"#,
                    usize::MAX,
                )
                .unwrap(),
            )
        })
    }
}
impl ListenerMain for Probe {
    fn definition_hash(&self) -> &str {
        "definition"
    }
    fn expected(&self, sender: &JsString) -> Json {
        Json::object([
            ("principalId", "bridge".into()),
            ("callerPrincipalId", Json::String(sender.clone())),
            ("workspaceId", "workspace".into()),
            ("definitionHash", "definition".into()),
        ])
    }
    fn is_notice_input<'a>(&'a self, id: &'a JsString) -> RequestFuture<'a> {
        Box::pin(async move {
            self.event(vec!["isNoticeInput".into(), Json::String(id.clone())]);
            let state = self.state.lock().unwrap();
            Ok(
                (f(&state.current, "canonicalNotice").as_bool() == Some(true)
                    || f(&state.current, "noticeInputs")
                        .as_array()
                        .is_some_and(|v| v.contains(&Json::String(id.clone()))))
                .into(),
            )
        })
    }
    fn operation_id(&self, scope: Json) -> RequestFuture<'_> {
        Box::pin(async move {
            self.event(vec!["operationId".into(), scope.clone()]);
            Ok(format!("synthetic-operation:{}", &hash_json(&scope)?[7..]).into())
        })
    }
    fn operation<'a>(&'a self, sender: &'a JsString, id: &'a Json) -> RequestFuture<'a> {
        Box::pin(async move {
            self.event(vec![
                "operation".into(),
                Json::String(sender.clone()),
                id.clone(),
            ]);
            self.take("operations", Json::Array(vec![Json::Object(vec![])]))
        })
    }
    fn history<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a> {
        Box::pin(async move {
            self.event(vec![
                "history".into(),
                Json::String(sender.clone()),
                request.clone(),
            ]);
            self.get(
                "history",
                Json::object([
                    ("entries", Json::Array(vec![default_reply()])),
                    ("throughSequence", Json::Number(3.0)),
                ]),
            )
        })
    }
    fn receive<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a> {
        Box::pin(async move {
            self.event(vec![
                "receive".into(),
                Json::String(sender.clone()),
                request.clone(),
            ]);
            self.take("receives", Json::Array(vec![received()]))
        })
    }
    fn acknowledge<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a> {
        Box::pin(async move {
            self.event(vec![
                "acknowledge".into(),
                Json::String(sender.clone()),
                request.clone(),
            ]);
            self.take(
                "acknowledgements",
                Json::Array(vec![Json::object([("phase", "succeeded".into())])]),
            )
        })
    }
    fn result<'a>(&'a self, reference: &'a Json, input: &'a Json) -> RequestFuture<'a> {
        Box::pin(async move {
            self.event(vec![
                "result".into(),
                "chat-bridge/result".into(),
                reference.clone(),
                input.clone(),
            ]);
            self.get(
                "result",
                Json::object([("value", Json::object([("turnId", "turn".into())]))]),
            )
        })
    }
    fn reply<'a>(&'a self, id: &'a Json, channel: &'a Json) -> RequestFuture<'a> {
        Box::pin(async move {
            self.event(vec!["reply".into(), id.clone(), channel.clone()]);
            Ok(Json::object([(
                "value",
                Json::object([("state", self.get("replyState", "pending".into())?)]),
            )]))
        })
    }
    fn pending_notices(&self, cursor: Option<JsString>) -> RequestFuture<'_> {
        Box::pin(async move {
            self.event(vec![
                "pendingNotices".into(),
                cursor.map(Json::String).unwrap_or(Json::Null),
            ]);
            self.take(
                "noticePages",
                Json::Array(vec![Json::object([
                    ("replies", Json::Array(vec![])),
                    ("nextCursor", Json::Null),
                ])]),
            )
        })
    }
    fn notice_parts<'a>(&'a self, first: &'a Json) -> RequestFuture<'a> {
        Box::pin(async move {
            self.event(vec!["noticeParts".into(), first.clone()]);
            self.get("noticeParts", Json::Array(vec![]))
        })
    }
    fn confirm_notice<'a>(&'a self, parts: &'a [Json]) -> RequestFuture<'a> {
        Box::pin(async move {
            self.event(vec!["confirmNotice".into(), Json::Array(parts.to_vec())]);
            self.take("confirmations", Json::Array(vec![Json::Null]))
        })
    }
}
fn config() -> Json {
    parse(r#"{"accountId":"account","botPhoneNumber":"+15555550200","senders":[{"phoneNumber":"+15555550201","principalId":"sender","channelId":"channel"}],"since":"2024-01-01T00:00:00Z"}"#,usize::MAX).unwrap()
}
fn make(probe: &Arc<Probe>, journal: &Journal) -> WhatsAppListener {
    let state = probe.state.lock().unwrap();
    let config = state.current.get("config").cloned().unwrap_or_else(config);
    let language = state.current.get("language").and_then(Json::as_js_string);
    WhatsAppListener::with_dependencies(
        probe.clone(),
        journal.clone(),
        probe.clone(),
        config,
        Translator::new(language),
        probe.clone(),
    )
}
async fn apply(
    action: &Json,
    listener: &mut WhatsAppListener,
    probe: &Arc<Probe>,
    journal: &Journal,
) -> IvyResult<Json> {
    let now = probe.utc_millis();
    match s(action, "op").as_str() {
        "tick" => {
            return Ok(listener
                .tick(f(action, "commentary").as_bool().unwrap_or(false))
                .await?
                .into());
        }
        "notification" => {
            return listener
                .notification(
                    f(action, "event"),
                    f(action, "commentary").as_bool().unwrap_or(false),
                )
                .await;
        }
        "outbox" => listener.outbox().await?,
        "notices" => listener.notices().await?,
        "patch" => probe.patch(f(action, "value")),
        "restart" => *listener = make(probe, journal),
        "drain" => {
            for _ in 0..4 {
                tokio::task::yield_now().await;
            }
        }
        "set" => journal.set(&s(action, "key"), f(action, "value"))?,
        "seedOutput" => {
            journal.access(|j| {
                let ids = j.enqueue(
                    &s(action, "key").into(),
                    "15555550201@s.whatsapp.net",
                    f(action, "text").as_js_string().unwrap(),
                    f(action, "automatic").as_bool().unwrap_or(false),
                    now,
                )?;
                if let Some(value) = action.get("state") {
                    for id in ids {
                        j.mark(
                            &id,
                            match value.as_str()?.as_str() {
                                "sent" => OutputState::Sent,
                                "unknown" => OutputState::Unknown,
                                _ => OutputState::Sending,
                            },
                        )?;
                    }
                }
                Ok(())
            })?;
        }
        "deliveryIds" => journal.set(
            &s(action, "key"),
            &Json::Array(
                journal.access(|j| Ok(j.outgoing()?.into_iter().map(|r| r.id.into()).collect()))?,
            ),
        )?,
        "mark" => journal.access(|j| {
            for row in j.outgoing()? {
                if action.get("text").is_none()
                    || f(action, "text").as_js_string() == Some(&JsString::from(row.text.as_str()))
                {
                    j.mark(
                        &row.id,
                        match action
                            .get("state")
                            .map(Json::as_str)
                            .transpose()?
                            .as_deref()
                            .unwrap_or("sent")
                        {
                            "sent" => OutputState::Sent,
                            "unknown" => OutputState::Unknown,
                            _ => OutputState::Sending,
                        },
                    )?;
                }
            }
            Ok(())
        })?,
        "prune" => journal.access(|j| j.prune(HistoryLimits { count: 0, bytes: 0 }))?,
        _ => panic!("Unknown listener action"),
    }
    Ok(Json::Null)
}
fn equal(actual: &Json, expected: &Json, label: &str) {
    assert_eq!(
        canonical(actual, usize::MAX).unwrap(),
        canonical(expected, usize::MAX).unwrap(),
        "{label}"
    );
}
#[tokio::test]
async fn original_listener_history_notifications_receipts_and_helpers_match() {
    let output = Command::new("node")
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .arg("tests/rust/fixtures/chat-whatsapp-listener-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oracle = parse(&String::from_utf8(output.stdout).unwrap(), usize::MAX).unwrap();
    let specs = f(&oracle, "specs").as_array().unwrap();
    let mut rows = Vec::new();
    for spec in specs {
        let journal = Journal::new(JournalIo::open(":memory:").unwrap());
        let probe = Arc::new(Probe {
            state: Mutex::new(State {
                current: spec.clone(),
                now: 1000,
                events: vec![],
                counts: HashMap::new(),
            }),
            start: Instant::now(),
        });
        let mut listener = make(&probe, &journal);
        if f(spec, "fresh").as_bool() == Some(true) {
            journal.set("listener-new:binding", &true.into()).unwrap();
        }
        if let Some(kv) = spec.get("kv").and_then(Json::as_object) {
            for (key, value) in kv {
                journal.set(&key.to_utf8().unwrap(), value).unwrap();
            }
        }
        if let Some(initial) = spec.get("initial").and_then(Json::as_array) {
            for action in initial {
                apply(action, &mut listener, &probe, &journal)
                    .await
                    .unwrap();
            }
        }
        probe.collect();
        let mut steps = Vec::new();
        for action in f(spec, "actions").as_array().unwrap() {
            let value = result(apply(action, &mut listener, &probe, &journal).await);
            steps.push(Json::object([
                ("result", value),
                ("events", Json::Array(probe.collect())),
                ("needsRecovery", listener.needs_recovery().into()),
                ("snapshot", journal.access(|j| Ok(snapshot(j))).unwrap()),
            ]));
        }
        journal.close().await.unwrap();
        rows.push(Json::object([
            ("label", f(spec, "label").clone()),
            ("steps", Json::Array(steps)),
        ]));
    }
    let mut helpers = Json::Object(vec![]);
    for kind in ["text", "terminal", "delegation"] {
        let mut values = Vec::new();
        for row in f(f(&oracle, "helpers"), kind).as_array().unwrap() {
            let mut actual = row.clone();
            let translator = Translator::new(row.get("language").and_then(Json::as_js_string));
            let value = match kind {
                "text" => whatsapp_text(f(row, "value").as_js_string().unwrap(), &translator)
                    .map(Json::String),
                "terminal" => terminal_text(f(row, "value"), &translator).map(Json::String),
                _ => delegation_source(f(row, "value"))
                    .map(|v| v.map(Json::String).unwrap_or(Json::Null)),
            };
            actual.insert("result", result(value));
            values.push(actual);
        }
        helpers.insert(kind, Json::Array(values));
    }
    let actual = Json::object([("rows", Json::Array(rows)), ("helpers", helpers)]);
    if let Some(path) = std::env::var_os("IVY_CHAT_LISTENER_ACTUAL_CAPTURE") {
        std::fs::write(path, stringify(&actual, usize::MAX).unwrap()).unwrap();
    }
    for (actual, expected) in f(&actual, "rows")
        .as_array()
        .unwrap()
        .iter()
        .zip(f(&oracle, "rows").as_array().unwrap())
    {
        equal(actual, expected, &s(expected, "label"));
    }
    equal(
        f(&actual, "helpers"),
        f(&oracle, "helpers"),
        "original listener UTF-16 text helpers",
    );
    eprintln!(
        "{} original listener scenarios and {} helper cases matched",
        specs.len(),
        f(&oracle, "helpers")
            .as_object()
            .unwrap()
            .iter()
            .map(|(_, v)| v.as_array().unwrap().len())
            .sum::<usize>()
    );
}
