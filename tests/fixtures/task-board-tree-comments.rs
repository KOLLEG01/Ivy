use ivy_contracts::{
    IvyJson as Json, JsString,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::task_comments::CancellationToken;
use ivy_sdk::task_comments::{CommentReadFuture, CommentSource};
use ivy_task_board::{
    engine_read::conversation::ConversationPolicies,
    journal_store::persistent_reads::task_comments::{CommentFailure, TaskComments},
};
use std::sync::{Arc, Mutex};
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn text(v: &Json) -> String {
    v.as_str().unwrap()
}
fn pin(id: &str) -> Json {
    Json::object([("objectId", id.into()), ("revision", Json::Number(1.0))])
}
fn leaf(id: &str) -> Json {
    Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("taskId", "task".into()),
        ("kind", "comment".into()),
        (
            "entry",
            Json::object([
                (
                    "comment",
                    Json::object([
                        ("commentId", id.into()),
                        ("sequence", Json::Number(1.0)),
                        ("authorKind", "agent".into()),
                        ("purpose", "update".into()),
                        ("body", Json::String(JsString(vec![0xd800, 0xd83d, 0xde00]))),
                        ("requests", Json::Array(vec![])),
                        ("responses", Json::Array(vec![])),
                    ]),
                ),
                ("receipt", Json::Null),
            ]),
        ),
    ])
}
struct RawFailure {
    value: Json,
    identity: Arc<()>,
}
struct Source {
    input: Json,
    trace: Mutex<Vec<Json>>,
    identity: Arc<()>,
}
impl CommentSource for Source {
    type Error = RawFailure;
    fn read<'a>(
        &'a self,
        p: Json,
        cancellation: Option<CancellationToken>,
    ) -> CommentReadFuture<'a, Self::Error> {
        Box::pin(async move {
            assert!(cancellation.is_none());
            self.trace.lock().unwrap().push(Json::object([
                ("method", "objects.read".into()),
                ("params", p.clone()),
            ]));
            if let Some(error) = self.input.get("error") {
                return Err(RawFailure {
                    value: error.clone(),
                    identity: self.identity.clone(),
                });
            }
            let id = text(f(&p, "objectId"));
            let node = if f(&self.input, "generated") == &true.into() {
                leaf(&id)
            } else {
                let key = format!("{}@{}", id, f(&p, "revision").as_f64().unwrap());
                f(&self.input, "nodes")
                    .get(&key)
                    .or_else(|| f(&self.input, "nodes").get(&id))
                    .expect("Unexpected source pin")
                    .clone()
            };
            let mut saved = Json::object([
                (
                    "revision",
                    Json::object([
                        ("contractVersion", "1.0.0".into()),
                        ("revision", f(&p, "revision").clone()),
                    ]),
                ),
                (
                    "content",
                    Json::object([("encoding", "json".into()), ("value", node)]),
                ),
            ]);
            if f(&self.input, "missingObject") != &true.into() {
                let mut object = Json::object([
                    ("contractKey", "task-board/comment-node".into()),
                    ("parentId", "task".into()),
                    ("ownerObjectId", "task".into()),
                ]);
                if let Some(fields) = f(&self.input, "metadata").as_object() {
                    for (k, v) in fields {
                        object.insert(k.clone(), v.clone());
                    }
                }
                saved.insert("object", object);
            }
            Ok(saved)
        })
    }
}
fn outcome(v: Result<Json, CommentFailure<RawFailure>>, identity: &Arc<()>) -> Json {
    match v {
        Ok(value) => Json::object([("value", value)]),
        Err(CommentFailure::Source(raw)) => {
            assert!(Arc::ptr_eq(&raw.identity, identity));
            Json::object([("source", raw.value), ("identity", true.into())])
        }
        Err(CommentFailure::Ivy(e)) => Json::object([("ivy", e.to_wire())]),
        Err(CommentFailure::JavaScript(e)) => Json::object([(
            "raw",
            Json::object([
                ("name", e.kind.into()),
                ("message", Json::String(e.message)),
            ]),
        )]),
    }
}
fn same(a: &Json, b: &Json, name: &str) {
    assert_eq!(
        canonical(a, MANAGEMENT_FRAME_BYTES).unwrap(),
        canonical(b, MANAGEMENT_FRAME_BYTES).unwrap(),
        "{name}"
    );
}
#[test]
fn actual_shared_tree_reader_drives_owned_hydration_and_caches() {
    let fixture = parse(
        &std::fs::read_to_string(std::env::var("IVY_TASK_BOARD_TREE_COMMENTS_FIXTURE").unwrap())
            .unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let validate = |_: &Json| panic!("Summary must not validate a next step");
        let now = || panic!("No clock is read");
        let condition = |_: &Json, _: Option<&Json>| panic!("No condition store is read");
        let policies = ConversationPolicies::current(&validate, &now, &condition);
        for case in f(&fixture, "cases").as_array().unwrap() {
            let input = f(case, "input");
            let before = input.clone();
            let source = Arc::new(Source {
                input: input.clone(),
                trace: Mutex::new(vec![]),
                identity: Arc::new(()),
            });
            let comments = TaskComments::new(source.clone(), runtime.handle().clone());
            let mut output = vec![];
            for action in f(input, "actions").as_array().unwrap() {
                let id = action
                    .get("taskId")
                    .and_then(Json::as_js_string)
                    .cloned()
                    .unwrap_or_else(|| "task".into());
                let result: Result<Json, CommentFailure<RawFailure>> = async {
                    Ok(match text(f(action, "kind")).as_str() {
                        "read" => comments
                            .read_tree(f(action, "task"), &id, &policies)
                            .await?
                            .expect("Tree case"),
                        "summary" => comments
                            .summary(f(action, "task"), f(f(action, "task"), "commentStore"), &id)?
                            .unwrap_or(Json::Null),
                        "range" => Json::Array(
                            comments
                                .range(
                                    f(action, "tree"),
                                    &id,
                                    Some(f(action, "start").as_f64().unwrap() as usize),
                                    Some(f(action, "end").as_f64().unwrap() as usize),
                                    None,
                                )
                                .await?,
                        ),
                        "node" => (*comments.node(f(action, "pin"), &id, None).await?).clone(),
                        "pin" => comments
                            .pin_for_hash(&hash_json(f(action, "node"))?)
                            .unwrap_or(Json::Null),
                        "summaryCapacity" => {
                            let tree = Json::object([
                                ("kind", "tree".into()),
                                ("root", Json::Null),
                                ("length", Json::Number(0.0)),
                            ]);
                            let task = Json::object([
                                ("nextSteps", Json::Array(vec![])),
                                ("commentStore", tree.clone()),
                            ]);
                            for i in 0..128 {
                                comments
                                    .read_tree(&task, &format!("t{i}").into(), &policies)
                                    .await?;
                            }
                            comments.read_tree(&task, &"t127".into(), &policies).await?;
                            let first = comments
                                .summary(&task, &tree, &"t0".into())?
                                .unwrap_or(Json::Null);
                            let second = comments
                                .summary(&task, &tree, &"t1".into())?
                                .unwrap_or(Json::Null);
                            comments.read_tree(&task, &"t0".into(), &policies).await?;
                            Json::object([
                                ("first", first),
                                ("second", second),
                                (
                                    "restored",
                                    comments
                                        .summary(&task, &tree, &"t0".into())?
                                        .unwrap_or(Json::Null),
                                ),
                                (
                                    "secondAfter",
                                    comments
                                        .summary(&task, &tree, &"t1".into())?
                                        .unwrap_or(Json::Null),
                                ),
                            ])
                        }
                        "pinCapacity" => {
                            for i in 0..16384 {
                                comments
                                    .node(&pin(&format!("n{i}")), &"task".into(), None)
                                    .await?;
                            }
                            comments.node(&pin("n16383"), &"task".into(), None).await?;
                            comments.node(&pin("n16384"), &"task".into(), None).await?;
                            let first = comments
                                .pin_for_hash(&hash_json(&leaf("n0"))?)
                                .unwrap_or(Json::Null);
                            let second = comments
                                .pin_for_hash(&hash_json(&leaf("n1"))?)
                                .unwrap_or(Json::Null);
                            comments.node(&pin("n0"), &"task".into(), None).await?;
                            Json::object([
                                ("first", first),
                                ("second", second),
                                (
                                    "restored",
                                    comments
                                        .pin_for_hash(&hash_json(&leaf("n0"))?)
                                        .unwrap_or(Json::Null),
                                ),
                                (
                                    "secondAfter",
                                    comments
                                        .pin_for_hash(&hash_json(&leaf("n1"))?)
                                        .unwrap_or(Json::Null),
                                ),
                            ])
                        }
                        _ => panic!("Unknown action"),
                    })
                }
                .await;
                output.push(outcome(result, &source.identity));
            }
            same(
                &Json::Array(output),
                f(case, "output"),
                &text(f(case, "name")),
            );
            let trace = source.trace.lock().unwrap();
            let actual = if f(input, "generated") == &true.into() {
                Json::object([
                    ("count", Json::Number(trace.len() as f64)),
                    (
                        "first",
                        Json::Array(trace.iter().take(2).cloned().collect()),
                    ),
                    (
                        "last",
                        Json::Array(
                            trace
                                .iter()
                                .skip(trace.len().saturating_sub(2))
                                .cloned()
                                .collect(),
                        ),
                    ),
                ])
            } else {
                Json::Array(trace.clone())
            };
            same(
                &actual,
                f(case, "trace"),
                &format!("{} RPC trace", text(f(case, "name"))),
            );
            assert_eq!(input, &before, "Source input unchanged");
        }
    });
}
