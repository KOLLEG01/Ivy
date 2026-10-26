#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use super::{Fixed, Fixture, array, equal, field, normalize, number, result, summary, text};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_chat_bridge::{
    evidence_bytes::save_chat_evidence,
    native_evidence::{
        ChatNativeEvidence, EvidenceFuture, NativeEvidenceValidation, verify_chat_native_operation,
        verify_chat_native_read, verify_chat_native_request,
    },
    native_plan::ChatNativeValidation,
    store::{ChatStore, Pin},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    checked_native_contract::CheckedNativeContracts,
    hash::{digest, hash_json},
    json::{canonical, parse},
    native_contract::NativeContract,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};
#[path = "chat-collector-cases.rs"]
mod collector;

fn member<'a>(value: &'a mut Json, key: &str) -> &'a mut Json {
    let Json::Object(fields) = value else {
        panic!("Expected fixture object")
    };
    &mut fields
        .iter_mut()
        .find(|(name, _)| name == &JsString::from(key))
        .unwrap()
        .1
}
fn merge(value: &mut Json, patch: &Json) {
    for (key, v) in patch.as_object().unwrap() {
        value.insert(key.clone(), v.clone());
    }
}
fn flag(value: &Json, key: &str) -> bool {
    value.get(key) == Some(&Json::Bool(true))
}
fn oracle() -> &'static Json {
    static ORACLE: OnceLock<Json> = OnceLock::new();
    ORACLE.get_or_init(|| {
        let output = reference_data::read(include_bytes!(
            "fixtures/chat-native-evidence-cases.json.gz"
        ));
        parse(std::str::from_utf8(&output).unwrap(), usize::MAX).unwrap()
    })
}
// Observe the actual production plan/compiler adapter. Original outcomes are
// comparison evidence only; they never supply a Rust validation result.
struct Gate {
    expected: Mutex<VecDeque<Json>>,
    calls: Mutex<Vec<Json>>,
}
impl Gate {
    fn new() -> Self {
        Self {
            expected: Mutex::new(VecDeque::new()),
            calls: Mutex::new(vec![]),
        }
    }
    fn begin(&self, expected: &[Json]) {
        assert!(self.expected.lock().unwrap().is_empty());
        *self.expected.lock().unwrap() = expected.iter().cloned().collect();
        self.calls.lock().unwrap().clear();
    }
    fn actual() -> ChatNativeValidation<'static> {
        static CATALOGS: OnceLock<ivy_contracts::checked_native_contract::CheckedNativeContracts> =
            OnceLock::new();
        static INSTRUCTIONS: OnceLock<JsString> = OnceLock::new();
        ChatNativeValidation::new(
            CATALOGS.get_or_init(|| {
                ivy_contracts::checked_native_contract::CheckedNativeContracts::new(
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
                )
            }),
            INSTRUCTIONS.get_or_init(|| {
                let bytes = STANDARD.decode(text(oracle(), "instructions")).unwrap();
                parse(std::str::from_utf8(&bytes).unwrap(), usize::MAX)
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .clone()
            }),
        )
    }
    fn observe(&self, kind: &str, input: Json, actual: IvyResult<Json>) -> IvyResult<Json> {
        let expected = self
            .expected
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("Unexpected {kind} gate"));
        assert_eq!(text(&expected, "kind"), kind);
        equal(
            &collector::compact(&input),
            &collector::compact(field(&expected, "input")),
            kind,
        );
        let output = result(actual.clone());
        equal(
            &collector::compact(&output),
            &collector::compact(field(&expected, "output")),
            &format!("actual {kind} validation"),
        );
        self.calls.lock().unwrap().push(Json::object([
            ("kind", kind.into()),
            ("input", input),
            ("output", output.clone()),
        ]));
        actual
    }
    fn contract(contract: &NativeContract) -> Json {
        Json::object([
            ("version", field(contract.catalog(), "version").clone()),
            (
                "sourceHash",
                field(contract.catalog(), "sourceHash").clone(),
            ),
            ("catalogHash", contract.catalog_hash().into()),
        ])
    }
}
impl NativeEvidenceValidation for Gate {
    fn validate_draft(&self, value: &Json) -> IvyResult<()> {
        self.observe(
            "draft",
            Json::object([("value", summary(value))]),
            Self::actual().validate_draft(value).map(|_| Json::Null),
        )
        .map(|_| ())
    }
    fn resolve_plan<'a>(
        &'a self,
        store: &'a ChatStore,
        plan: &'a Json,
    ) -> EvidenceFuture<'a, Json> {
        Box::pin(async move {
            self.observe(
                "plan",
                Json::object([("plan", plan.clone())]),
                Self::actual().resolve_plan(store, plan).await,
            )
        })
    }
    fn validate_input(
        &self,
        contract: &NativeContract,
        method: &JsString,
        params: &Json,
        maximum_bytes: usize,
    ) -> IvyResult<()> {
        self.observe(
            "input",
            Json::object([
                ("contract", Self::contract(contract)),
                ("method", Json::String(method.clone())),
                ("params", summary(params)),
                ("maximumBytes", Json::Number(maximum_bytes as f64)),
            ]),
            Self::actual()
                .validate_input(contract, method, params, maximum_bytes)
                .map(|_| Json::Null),
        )
        .map(|_| ())
    }
    fn validate_payload(
        &self,
        contract: &NativeContract,
        observation: &Json,
        request_bytes: usize,
        response_bytes: usize,
    ) -> IvyResult<()> {
        self.observe(
            "payload",
            Json::object([
                ("contract", Self::contract(contract)),
                ("observation", summary(observation)),
                (
                    "limits",
                    Json::object([
                        ("requestBytes", Json::Number(request_bytes as f64)),
                        ("responseBytes", Json::Number(response_bytes as f64)),
                    ]),
                ),
            ]),
            Self::actual()
                .validate_payload(contract, observation, request_bytes, response_bytes)
                .map(|_| Json::Null),
        )
        .map(|_| ())
    }
}
struct State {
    input: Json,
    calls: Vec<Json>,
    unavailable: bool,
    stat_patch: Json,
}
struct Client(Mutex<State>);
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            state.calls.push(Json::object([
                ("method", method.into()),
                ("params", params.clone()),
            ]));
            if state.unavailable {
                return Err(IvyError::new(
                    "fixture_owner_unavailable",
                    "Original owner read unavailable.",
                )
                .with_outcome(Outcome::Unknown));
            }
            if params.get("objectId") != Some(&"original-input".into()) {
                return Err(IvyError::new(
                    "object_missing",
                    "Original parent is unavailable.",
                ));
            }
            match method {
                "objects.read" => Ok(state.input.clone()),
                "objects.stat" => {
                    let mut metadata = field(&state.input, "object").clone();
                    merge(&mut metadata, &state.stat_patch);
                    Ok(metadata)
                }
                _ => panic!("Unexpected RPC {method}"),
            }
        })
    }
}
fn bodies(case: &Json) -> Json {
    let mut body = field(case, "body").clone();
    let spec = case.get("spec");
    if spec.is_some_and(|s| flag(s, "large")) {
        let mut params = field(field(&body, "request"), "params").clone();
        params.insert(
            "input",
            Json::Array(vec![Json::object([
                ("type", "image".into()),
                (
                    "url",
                    format!(
                        "data:image/png;base64,{}",
                        STANDARD.encode(vec![31u8; 4 * 1024 * 1024])
                    )
                    .into(),
                ),
            ])]),
        );
        member(&mut body, "request").insert("params", params.clone());
        member(&mut body, "operation").insert("params", params);
        let result = member(member(member(&mut body, "operation"), "reply"), "result");
        let turn = member(result, "turn");
        let mut items = array(turn, "items").to_vec();
        items[0].insert("text", "r".repeat(5 * 1024 * 1024).into());
        turn.insert("items", Json::Array(items));
    }
    if let Some(length) = spec
        .and_then(|s| s.get("envelopeBytes"))
        .and_then(Json::as_f64)
    {
        let mut params = field(field(&body, "request"), "params").clone();
        let base = canonical(
            &Json::object([
                ("id", "00000000-0000-0000-0000-000000000000".into()),
                ("method", field(field(&body, "request"), "method").clone()),
                ("params", params.clone()),
            ]),
            usize::MAX,
        )
        .unwrap()
        .len();
        let mut input = array(&params, "input").to_vec();
        input[0].insert("text", "x".repeat(length as usize - base).into());
        params.insert("input", Json::Array(input));
        member(&mut body, "request").insert("params", params.clone());
        member(&mut body, "operation").insert("params", params);
    }
    body
}
fn snapshot(
    path: &Path,
    client: &Client,
    gate: &Gate,
    output: Json,
    last: &mut i64,
    aliases: &mut HashMap<String, String>,
) -> Json {
    let db = rusqlite::Connection::open(path.join("chat-bridge.sqlite")).unwrap();
    let documents:Vec<_>=db.prepare("SELECT d.id,d.name,d.parent_id,d.current_revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='chat-bridge/evidence' ORDER BY d.name").unwrap()
        .query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,i64>(3)?,r.get::<_,String>(4)?))).unwrap().map(|r|{let(id,name,parent,revision,value)=r.unwrap();aliases.insert(id.clone(),format!("<local:{name}>"));Json::object([
            ("id",id.into()),("name",name.into()),("parentId",parent.map(Json::from).unwrap_or(Json::Null)),("revision",Json::Number(revision as f64)),("value",parse(&value,usize::MAX).unwrap())])}).collect();
    let blobs = db
        .prepare("SELECT id,key,name,parent_id,bytes FROM blobs ORDER BY name,parent_id")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Vec<u8>>(4)?,
            ))
        })
        .unwrap()
        .map(|r| {
            let (id, key, name, parent, bytes) = r.unwrap();
            aliases.insert(id.clone(), format!("<local:{name}>"));
            Json::object([
                ("id", id.into()),
                ("key", key.into()),
                ("name", name.into()),
                ("parentId", parent.map(Json::from).unwrap_or(Json::Null)),
                ("byteLength", Json::Number(bytes.len() as f64)),
                ("contentHash", digest(bytes).into()),
            ])
        })
        .collect();
    let writes = db
        .prepare(
            "SELECT rowid,value_json FROM native_evidence_test_writes WHERE rowid>? ORDER BY rowid",
        )
        .unwrap()
        .query_map([*last], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|r| {
            let (id, value) = r.unwrap();
            *last = id;
            parse(&value, usize::MAX).unwrap()
        })
        .collect();
    assert!(
        gate.expected.lock().unwrap().is_empty(),
        "Missing required shared gate"
    );
    let calls = std::mem::take(&mut client.0.lock().unwrap().calls);
    let gates = std::mem::take(&mut *gate.calls.lock().unwrap());
    normalize(
        &Json::object([
            ("output", output),
            ("gates", Json::Array(gates)),
            ("calls", Json::Array(calls)),
            ("documents", Json::Array(documents)),
            ("blobs", Json::Array(blobs)),
            ("writes", Json::Array(writes)),
        ]),
        aliases,
    )
}
fn selected_call(body: &Json, pins: &HashMap<String, Pin>) -> Json {
    let mut call = field(body, "call").clone();
    if let Some(request) = pins.get("request") {
        call.insert("request", request.json());
    }
    call
}
fn selected_pin<'a>(pins: &'a HashMap<String, Pin>, name: &str) -> IvyResult<&'a Pin> {
    pins.get(name).ok_or_else(|| {
        IvyError::new(
            "fixture_pin_missing",
            "Original evidence pin is unavailable.",
        )
    })
}
async fn step(
    action: &Json,
    store: &ChatStore,
    definition: &mut Json,
    body: &mut Json,
    pins: &mut HashMap<String, Pin>,
    evidence: &ChatNativeEvidence<'_>,
    client: &Client,
    gate: &Gate,
    catalogs: &CheckedNativeContracts,
    instructions: &JsString,
    parent: &str,
) -> IvyResult<Json> {
    match text(action, "action").as_str() {
        "patch" => {
            let target = text(action, "target");
            if target == "input" {
                merge(
                    member(
                        member(&mut client.0.lock().unwrap().input, "content"),
                        "value",
                    ),
                    field(action, "value"),
                );
            } else {
                merge(member(body, &target), field(action, "value"));
            }
        }
        "verify-request" => {
            verify_chat_native_request(
                store,
                definition,
                field(body, "call"),
                field(body, "request"),
                catalogs,
                gate,
                instructions,
            )
            .await?
        }
        "verify-operation" => {
            return Ok(summary(
                verify_chat_native_operation(
                    store,
                    definition,
                    field(body, "call"),
                    field(body, "request"),
                    field(body, "operation"),
                    catalogs,
                    gate,
                    instructions,
                )
                .await?,
            ));
        }
        "verify-read" => verify_chat_native_read(
            field(body, "read"),
            field(body, "expected"),
            Some(field(field(definition, "nativePlan"), "catalogSourceHash")),
            catalogs,
            gate,
        )?,
        "save-request" => {
            let saved = evidence
                .save_request(parent, field(body, "request"))
                .await?;
            pins.insert("request".into(), saved.clone());
            return Ok(saved.json());
        }
        "request" => {
            return Ok(summary(
                &evidence.request(parent, &selected_call(body, pins)).await?,
            ));
        }
        "save-operation" => {
            let saved = evidence
                .save_operation(parent, &selected_call(body, pins), field(body, "operation"))
                .await?;
            pins.insert("operation".into(), saved.clone());
            return Ok(saved.json());
        }
        "operation" => {
            return Ok(summary(
                &evidence
                    .operation(
                        parent,
                        &selected_call(body, pins),
                        selected_pin(pins, "operation")?,
                    )
                    .await?,
            ));
        }
        "save-read" => {
            let saved = evidence
                .save_read(parent, field(body, "expected"), field(body, "read"))
                .await?;
            pins.insert("read".into(), saved.clone());
            return Ok(saved.json());
        }
        "read" => {
            return Ok(summary(
                &evidence
                    .read(parent, field(body, "expected"), selected_pin(pins, "read")?)
                    .await?,
            ));
        }
        "raw-operation" | "raw-read" => {
            let read = text(action, "action") == "raw-read";
            let format = if read {
                "agent-read-observation/canonical-json"
            } else {
                "agent-operation/canonical-json"
            };
            let key = if read { "read" } else { "operation" };
            let saved = save_chat_evidence(store, parent, format, field(body, key), &|v| {
                gate.validate_draft(v)
            })
            .await?;
            pins.insert(key.into(), saved);
        }
        "remote-error" => client.0.lock().unwrap().unavailable = true,
        "change-settings" => {
            member(member(body, "request"), "params").insert("model", "unplanned".into());
            let request = field(body, "request");
            let hash = hash_json(&Json::object([
                ("method", field(request, "method").clone()),
                ("params", field(request, "params").clone()),
            ]))?;
            member(body, "call").insert("requestHash", hash.into());
        }
        "construct-other-root" => {
            let other = ChatStore::with_clock(
                store.client.clone(),
                Some("other".into()),
                None,
                Arc::new(Fixed(1790000000123)),
            )
            .await
            .map_err(|e| e.into_ivy())?;
            ChatNativeEvidence::new(&other, definition, catalogs, gate, instructions)?;
        }
        "change-plan-identity" => member(member(definition, "nativePlan"), "definitions")
            .insert("threadStart", digest("changed-plan-definition").into()),
        "mutate-definition" => {
            definition.insert("workspaceId", "changed-after-construction".into())
        }
        "reopen" => {}
        _ => panic!("Unknown action"),
    }
    Ok(Json::Null)
}
#[tokio::test]
async fn native_evidence_scope_identity_template_and_required_gates_match_original() {
    let corpus = oracle();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let catalogs = CheckedNativeContracts::new(root);
    let bytes = STANDARD.decode(text(corpus, "instructions")).unwrap();
    let instructions = parse(std::str::from_utf8(&bytes).unwrap(), usize::MAX)
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone();
    for case in array(corpus, "cases") {
        let seed = field(field(corpus, "seeds"), &text(case, "version"));
        let path = Fixture::new(field(seed, "database"));
        let mut definition = field(seed, "definition").clone();
        let mut input = field(seed, "input").clone();
        if let Some(patch) = case.get("inputPatch") {
            merge(member(member(&mut input, "content"), "value"), patch);
        }
        if case.get("spec").is_some_and(|s| flag(s, "notice")) {
            let value = member(member(&mut input, "content"), "value");
            let identity = member(value, "identity");
            identity.insert("senderPrincipalId", "producer".into());
            identity.insert("messageId", "chat:1790000000123:notice".into());
            value.insert("operationId", "chat:1790000000123:notice".into());
        }
        let client = Arc::new(Client(Mutex::new(State {
            input,
            calls: vec![],
            unavailable: false,
            stat_patch: case
                .get("statPatch")
                .cloned()
                .unwrap_or(Json::Object(vec![])),
        })));
        let gate = Gate::new();
        let mut body = bodies(case);
        let mut pins = HashMap::new();
        let mut aliases = HashMap::new();
        let mut last = 0;
        let mut index = 0;
        let parent = match case.get("parent").and_then(|v| v.as_str().ok()).as_deref() {
            Some("operation") => text(field(seed, "send"), "objectId"),
            Some("missing") => "missing".into(),
            _ => "original-input".into(),
        };
        while index < array(case, "steps").len() {
            let store = ChatStore::with_clock(
                client.clone(),
                None,
                Some(path.0.clone()),
                Arc::new(Fixed(number(corpus, "now") as i64)),
            )
            .await
            .unwrap();
            let evidence =
                ChatNativeEvidence::new(&store, &definition, &catalogs, &gate, &instructions)
                    .unwrap();
            loop {
                let action = &array(case, "steps")[index];
                let expected = &array(case, "expected")[index];
                gate.begin(array(expected, "gates"));
                let output = result(
                    step(
                        action,
                        &store,
                        &mut definition,
                        &mut body,
                        &mut pins,
                        &evidence,
                        &client,
                        &gate,
                        &catalogs,
                        &instructions,
                        &parent,
                    )
                    .await,
                );
                let actual = snapshot(&path.0, &client, &gate, output, &mut last, &mut aliases);
                equal(
                    &actual,
                    expected,
                    &format!(
                        "{} step {index} {}",
                        text(case, "label"),
                        text(action, "action")
                    ),
                );
                index += 1;
                if index == array(case, "steps").len() || text(action, "action") == "reopen" {
                    break;
                }
            }
        }
    }
}
