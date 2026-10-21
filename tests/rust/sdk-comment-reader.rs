use ivy_contracts::{
    IvyJson as J, JsString,
    hash::digest,
    json::{parse, stringify},
};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    task_comments::*,
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;

// Deliberately implements none of Clone/Debug/Display/Error.
struct Raw {
    value: J,
    identity: Arc<()>,
}
type Read = dyn Fn(J, Option<CancellationToken>) -> CommentReadFuture<'static, Raw> + Send + Sync;
struct Source(Arc<Read>);
impl CommentSource for Source {
    type Error = Raw;
    fn read<'a>(&'a self, pin: J, signal: Option<CancellationToken>) -> CommentReadFuture<'a, Raw> {
        (self.0)(pin, signal)
    }
}
fn source(
    f: impl Fn(J, Option<CancellationToken>) -> CommentReadFuture<'static, Raw> + Send + Sync + 'static,
) -> Arc<Source> {
    Arc::new(Source(Arc::new(f)))
}
fn json(bytes: &[u8]) -> J {
    parse(std::str::from_utf8(bytes).unwrap(), usize::MAX).unwrap()
}
fn pin(id: &str) -> J {
    J::object([("objectId", id.into()), ("revision", J::Number(1.0))])
}
fn index(id: &str, length: usize) -> J {
    J::object([("object", pin(id)), ("length", J::Number(length as f64))])
}
fn tree(id: &str, length: usize) -> J {
    J::object([
        ("kind", "tree".into()),
        ("root", index(id, length)),
        ("length", J::Number(length as f64)),
    ])
}
fn leaf(sequence: usize) -> J {
    J::object([
        ("schemaVersion", J::Number(1.0)),
        ("taskId", "task".into()),
        ("kind", "comment".into()),
        (
            "entry",
            J::object([
                (
                    "comment",
                    J::object([
                        ("commentId", format!("c{sequence}").into()),
                        ("sequence", J::Number(sequence as f64)),
                        ("body", "value".into()),
                    ]),
                ),
                ("receipt", J::Null),
            ]),
        ),
    ])
}
fn page(children: Vec<J>) -> J {
    J::object([
        ("schemaVersion", J::Number(1.0)),
        ("taskId", "task".into()),
        ("kind", "page".into()),
        ("children", J::Array(children)),
    ])
}
fn saved(pin: &J, value: J) -> J {
    J::object([
        (
            "object",
            J::object([
                ("id", pin.get("objectId").unwrap().clone()),
                ("contractKey", COMMENT_NODE_KEY.into()),
                ("parentId", "task".into()),
                ("ownerObjectId", "task".into()),
            ]),
        ),
        (
            "revision",
            J::object([
                ("revision", pin.get("revision").unwrap().clone()),
                ("contractVersion", "1.0.0".into()),
            ]),
        ),
        (
            "content",
            J::object([("encoding", "json".into()), ("value", value)]),
        ),
    ])
}
fn task() -> JsString {
    "task".into()
}
fn id(pin: &J) -> String {
    pin.get("objectId").unwrap().as_str().unwrap()
}
fn mutate<'a>(value: &'a mut J, path: &[&str]) -> &'a mut J {
    if path.is_empty() {
        return value;
    }
    let J::Object(pairs) = value else {
        panic!("not an object")
    };
    let (_, value) = pairs
        .iter_mut()
        .find(|(k, _)| *k == JsString::from(path[0]))
        .unwrap();
    mutate(value, &path[1..])
}
fn raw(value: J) -> Raw {
    Raw {
        value,
        identity: Arc::new(()),
    }
}
fn failure(error: CommentError<Raw>) -> J {
    match error {
        CommentError::JavaScript(e) => J::object([
            ("type", "object".into()),
            ("name", e.kind.into()),
            ("message", J::String(e.message)),
            ("value", J::object([] as [(&str, J); 0])),
        ]),
        CommentError::Source(e) => {
            let kind = match &e.value {
                J::String(_) => "string",
                J::Number(_) => "number",
                J::Bool(_) => "boolean",
                _ => "object",
            };
            J::object([
                ("type", kind.into()),
                ("name", e.value.get("name").cloned().unwrap_or(J::Null)),
                (
                    "message",
                    e.value.get("message").cloned().unwrap_or(J::Null),
                ),
                ("value", e.value),
            ])
        }
    }
}
fn ok<T>(value: Result<T, CommentError<Raw>>) -> T {
    match value {
        Ok(v) => v,
        Err(e) => panic!("{}", stringify(&failure(e), usize::MAX).unwrap()),
    }
}
fn evidence(name: &str, value: &J) {
    if let Some(dir) = std::env::var_os("IVY_TEST_RAW_EVIDENCE") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::Path::new(&dir).join(name),
            stringify(value, usize::MAX).unwrap(),
        )
        .unwrap();
    }
}
fn fixture() -> J {
    json(include_bytes!(
        "../fixtures/sdk-comment-reader-original.json"
    ))
}

async fn observed(name: &str, trace: Arc<Mutex<Vec<J>>>) -> Result<J, CommentError<Raw>> {
    let names = [
        "valid immutable pin",
        "wrong contract key",
        "wrong contract version",
        "wrong parent",
        "wrong owner",
        "non-json content",
        "wrong revision",
        "wrong task id",
        "object absent raw TypeError",
        "revision null raw TypeError",
        "content value null raw TypeError",
        "object id is not independently compared",
        "schemaVersion is not independently validated",
    ];
    if let Some(case) = names.iter().position(|n| *n == name) {
        let p = pin("n");
        let mut response = saved(&p, leaf(1));
        match case {
            1 => *mutate(&mut response, &["object", "contractKey"]) = "other".into(),
            2 => *mutate(&mut response, &["revision", "contractVersion"]) = "1.0.1".into(),
            3 => *mutate(&mut response, &["object", "parentId"]) = "other".into(),
            4 => *mutate(&mut response, &["object", "ownerObjectId"]) = "other".into(),
            5 => *mutate(&mut response, &["content", "encoding"]) = "text".into(),
            6 => *mutate(&mut response, &["revision", "revision"]) = J::Number(2.0),
            7 => *mutate(&mut response, &["content", "value", "taskId"]) = "other".into(),
            8 => {
                response.remove("object");
            }
            9 => *mutate(&mut response, &["revision"]) = J::Null,
            10 => *mutate(&mut response, &["content", "value"]) = J::Null,
            11 => *mutate(&mut response, &["object", "id"]) = "other".into(),
            12 => *mutate(&mut response, &["content", "value", "schemaVersion"]) = J::Number(2.0),
            _ => {}
        }
        let signal = CancellationToken::new();
        let test_signal = signal.clone();
        let reader = CommentReader::new(
            source(move |p, s| {
                let response = response.clone();
                let trace = trace.clone();
                let test_signal = test_signal.clone();
                Box::pin(async move {
                    let signal = s.unwrap();
                    signal.cancel();
                    assert!(test_signal.is_cancelled());
                    trace.lock().unwrap().push(J::object([
                        ("method", "objects.read".into()),
                        ("params", p),
                        ("hasSignal", true.into()),
                    ]));
                    Ok(response)
                })
            }),
            tokio::runtime::Handle::current(),
        );
        let node = reader.node(&p, &task(), Some(&signal)).await?;
        assert!(Arc::ptr_eq(
            &node,
            &ok(reader.node(&p, &task(), None).await)
        ));
        return Ok(J::object([("node", node.as_ref().clone())]));
    }
    if name.starts_with("request exception") || name.starts_with("request primitive") {
        let value = if name.starts_with("request exception") {
            json(br#"{"name":"CustomError","message":"raw\ud800","code":"specific","details":{"a":1}}"#)
        } else {
            J::String(JsString(vec![114, 97, 119, 0xd800]))
        };
        let identity = Arc::new(());
        let original = identity.clone();
        let reader = CommentReader::new(
            source(move |_, _| {
                let value = value.clone();
                let identity = identity.clone();
                Box::pin(async move { Err(Raw { value, identity }) })
            }),
            tokio::runtime::Handle::current(),
        );
        let error = match reader.node(&pin("n"), &task(), None).await {
            Err(CommentError::Source(e)) => e,
            _ => panic!("raw failure lost"),
        };
        assert!(Arc::ptr_eq(&original, &error.identity));
        if name.starts_with("request exception") {
            trace
                .lock()
                .unwrap()
                .push(J::object([("same", true.into())]));
        }
        return Err(CommentError::Source(error));
    }
    if name == "cached hit ignores cancelled signal but checks task" {
        let reader = CommentReader::new(
            source(|_, _| Box::pin(async { panic!("cache hit requested") })),
            tokio::runtime::Handle::current(),
        );
        let node = Arc::new(leaf(1));
        let p = pin("cached");
        ok(reader.remember(&p, node.clone()));
        let token = CancellationToken::new();
        token.cancel();
        let got = ok(reader.node(&p, &task(), Some(&token)).await);
        trace.lock().unwrap().push(J::object([(
            "sameReference",
            Arc::ptr_eq(&got, &node).into(),
        )]));
        reader.node(&p, &"other".into(), Some(&token)).await?;
        unreachable!()
    }
    if name.starts_with("remember is")
        || name.starts_with("oversized new")
        || name.starts_with("cache bytes")
    {
        return cache_case(name).await;
    }
    if name == "empty tree"
        || name == "end exceeds length"
        || name == "start after end"
        || name == "root length mismatch"
    {
        let reader = CommentReader::new(
            source(|_, _| Box::pin(async { panic!("invalid range requested") })),
            tokio::runtime::Handle::current(),
        );
        let t = if name == "empty tree" {
            J::object([
                ("kind", "tree".into()),
                ("root", J::Null),
                ("length", J::Number(0.0)),
            ])
        } else {
            tree("root", 2)
        };
        let mut t = t;
        if name == "root length mismatch" {
            *mutate(&mut t, &["root", "length"]) = J::Number(1.0);
        }
        let (start, end) = match name {
            "end exceeds length" => (Some(0), Some(3)),
            "start after end" => (Some(2), Some(1)),
            "root length mismatch" => (Some(0), Some(1)),
            _ => (None, None),
        };
        return reader
            .range(&t, &task(), start, end, None)
            .await
            .map(J::Array);
    }
    let bad = match name {
        "leaf wrong sequence" => Some((leaf(2), 1)),
        "leaf wrong index length" => Some((leaf(1), 2)),
        "empty children" => Some((page(vec![]), 1)),
        "incomplete children sum" => Some((page(vec![index("leaf", 1)]), 2)),
        "missing children raw TypeError" => Some((
            J::object([
                ("schemaVersion", J::Number(1.0)),
                ("taskId", "task".into()),
                ("kind", "other".into()),
            ]),
            1,
        )),
        _ => None,
    };
    if let Some((root, length)) = bad {
        let reader = CommentReader::new(
            source(move |p, _| {
                let trace = trace.clone();
                let root = root.clone();
                Box::pin(async move {
                    trace.lock().unwrap().push(J::object([
                        ("method", "objects.read".into()),
                        ("p", p.clone()),
                    ]));
                    Ok(saved(&p, root))
                })
            }),
            tokio::runtime::Handle::current(),
        );
        return reader
            .range(&tree("root", length), &task(), None, None, None)
            .await
            .map(J::Array);
    }
    if name == "depth thirteen refuses before node request" {
        let reader = CommentReader::new(
            source(move |p, _| {
                let trace = trace.clone();
                Box::pin(async move {
                    let key = id(&p);
                    trace.lock().unwrap().push(key.clone().into());
                    let depth = key[1..].parse::<usize>().unwrap();
                    Ok(saved(&p, page(vec![index(&format!("d{}", depth + 1), 1)])))
                })
            }),
            tokio::runtime::Handle::current(),
        );
        return reader
            .range(&tree("d0", 1), &task(), None, None, None)
            .await
            .map(J::Array);
    }
    if name == "range follows only intersections and returns independent clones" {
        let mut c3 = leaf(3);
        *mutate(&mut c3, &["entry", "comment", "body"]) =
            J::String(JsString(vec![0xd800, 0xd83d, 0xde00]));
        mutate(&mut c3, &["entry", "comment"]).insert("unknown", J::Number(17.0));
        let docs = HashMap::from([
            (
                "root".to_owned(),
                page(vec![index("left", 2), index("right", 2)]),
            ),
            (
                "right".to_owned(),
                page(vec![index("c3", 1), index("c4", 1)]),
            ),
            ("c3".to_owned(), c3),
            ("c4".to_owned(), leaf(4)),
        ]);
        let reader = CommentReader::new(
            source(move |p, _| {
                let trace = trace.clone();
                let value = docs[&id(&p)].clone();
                Box::pin(async move {
                    trace.lock().unwrap().push(id(&p).into());
                    Ok(saved(&p, value))
                })
            }),
            tokio::runtime::Handle::current(),
        );
        let mut first = ok(reader
            .range(&tree("root", 4), &task(), Some(2), Some(3), None)
            .await);
        *mutate(&mut first[0], &["comment", "body"]) = "changed".into();
        return reader
            .range(&tree("root", 4), &task(), Some(2), Some(3), None)
            .await
            .map(J::Array);
    }
    if name.starts_with("micro-batches") {
        return batch_case(trace).await;
    }
    if name.starts_with("fail-fast") {
        return fail_fast_case(trace).await;
    }
    panic!("unhandled original case {name}")
}

async fn cache_case(name: &str) -> Result<J, CommentError<Raw>> {
    let mut node = leaf(1);
    if name.starts_with("cache bytes") {
        let comment = mutate(&mut node, &["entry", "comment"]);
        comment.insert("body", J::String(JsString(vec![0xd800, 0xd83d, 0xde00])));
        comment.insert("extra", J::object([("0", "v".into())]));
    }
    let original = fixture();
    let row = original
        .get("cases")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row.get("name").unwrap().as_str().unwrap() == name)
        .unwrap();
    let trace = &row.get("trace").unwrap().as_array().unwrap()[0];
    let size = if name.starts_with("remember is") {
        trace.get("size").unwrap().as_f64().unwrap() as usize
    } else if name.starts_with("oversized new") {
        trace.get("maximum").unwrap().as_f64().unwrap() as usize / 2
    } else {
        trace.get("charged").unwrap().as_f64().unwrap() as usize
    };
    assert_eq!(
        stringify(&node, usize::MAX).unwrap().encode_utf16().count() * 2,
        size,
        "retained original JSON.stringify byte charge"
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let log = calls.clone();
    let normal = node.clone();
    let factory = source(move |p, _| {
        let calls = log.clone();
        let normal = normal.clone();
        Box::pin(async move {
            calls.lock().unwrap().push(id(&p));
            Ok(saved(&p, normal))
        })
    });
    if name.starts_with("cache bytes") {
        let reader = CommentReader::with_maximum_bytes(
            factory.clone(),
            tokio::runtime::Handle::current(),
            size,
        );
        let node = Arc::new(node);
        ok(reader.remember(&pin("utf16"), node.clone()));
        assert!(Arc::ptr_eq(
            &node,
            &ok(reader.node(&pin("utf16"), &task(), None).await)
        ));
        assert!(calls.lock().unwrap().is_empty());
        let smaller =
            CommentReader::with_maximum_bytes(factory, tokio::runtime::Handle::current(), size - 1);
        ok(smaller.remember(&pin("utf16"), node));
        ok(smaller.node(&pin("utf16"), &task(), None).await);
        assert_eq!(calls.lock().unwrap().len(), 1);
        return Ok(true.into());
    }
    let reader =
        CommentReader::with_maximum_bytes(factory, tokio::runtime::Handle::current(), size * 2);
    let first = Arc::new(node);
    ok(reader.remember(&pin("a"), first.clone()));
    ok(reader.remember(&pin("b"), first.clone()));
    if name.starts_with("remember is") {
        ok(reader.remember(&pin("a"), Arc::new(leaf(2))));
        assert!(Arc::ptr_eq(
            &first,
            &ok(reader.node(&pin("a"), &task(), None).await)
        ));
        ok(reader.remember(&pin("c"), first.clone()));
        for key in ["b", "c"] {
            assert!(Arc::ptr_eq(
                &first,
                &ok(reader.node(&pin(key), &task(), None).await)
            ));
        }
        assert!(calls.lock().unwrap().is_empty());
        ok(reader.node(&pin("a"), &task(), None).await);
        assert_eq!(*calls.lock().unwrap(), vec!["a"]);
        Ok(J::object([("firstWins", true.into())]))
    } else {
        let mut huge = leaf(2);
        *mutate(&mut huge, &["entry", "comment", "body"]) = "x".repeat(size * 2).into();
        ok(reader.remember(&pin("huge"), Arc::new(huge)));
        for key in ["a", "b", "huge"] {
            ok(reader.node(&pin(key), &task(), None).await);
        }
        assert_eq!(*calls.lock().unwrap(), vec!["a", "b", "huge"]);
        Ok(true.into())
    }
}

async fn batch_case(trace: Arc<Mutex<Vec<J>>>) -> Result<J, CommentError<Raw>> {
    type Gate = (oneshot::Sender<()>, oneshot::Receiver<()>);
    let waiting = Arc::new(Mutex::new(Vec::<Gate>::new()));
    let notify = Arc::new(Notify::new());
    let count = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let w = waiting.clone();
    let n = notify.clone();
    let driver = tokio::spawn(async move {
        for size in [8, 8, 1] {
            let wave = loop {
                let change = n.notified();
                if w.lock().unwrap().len() == size {
                    break std::mem::take(&mut *w.lock().unwrap());
                }
                change.await;
            };
            for (send, ack) in wave.into_iter().rev() {
                send.send(()).unwrap();
                ack.await.unwrap();
            }
        }
    });
    let max = maximum.clone();
    let reader = CommentReader::new(
        source(move |p, _| {
            let (trace, waiting, notify, count, maximum) = (
                trace.clone(),
                waiting.clone(),
                notify.clone(),
                count.clone(),
                maximum.clone(),
            );
            Box::pin(async move {
                let key = id(&p);
                trace
                    .lock()
                    .unwrap()
                    .push(J::object([("start", key.clone().into())]));
                if key == "root" {
                    return Ok(saved(
                        &p,
                        page((1..=17).map(|n| index(&format!("c{n}"), 1)).collect()),
                    ));
                }
                let active = count.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(active, Ordering::SeqCst);
                let (send, wait) = oneshot::channel();
                let (ack, done) = oneshot::channel();
                waiting.lock().unwrap().push((send, done));
                notify.notify_one();
                wait.await.unwrap();
                trace
                    .lock()
                    .unwrap()
                    .push(J::object([("finish", key.clone().into())]));
                count.fetch_sub(1, Ordering::SeqCst);
                let _ = ack.send(());
                Ok(saved(&p, leaf(key[1..].parse().unwrap())))
            })
        }),
        tokio::runtime::Handle::current(),
    );
    let result = reader
        .range(&tree("root", 17), &task(), None, None, None)
        .await?;
    driver.await.unwrap();
    assert_eq!(max.load(Ordering::SeqCst), 8);
    Ok(J::object([
        ("maximum", J::Number(8.0)),
        (
            "sequences",
            J::Array(
                result
                    .iter()
                    .map(|entry| {
                        entry
                            .get("comment")
                            .unwrap()
                            .get("sequence")
                            .unwrap()
                            .clone()
                    })
                    .collect(),
            ),
        ),
    ]))
}
async fn fail_fast_case(trace: Arc<Mutex<Vec<J>>>) -> Result<J, CommentError<Raw>> {
    let (release, slow) = oneshot::channel();
    let slow = Arc::new(Mutex::new(Some(slow)));
    let completed = Arc::new(Notify::new());
    let after = completed.clone();
    let seen = Arc::new(AtomicUsize::new(0));
    let observed = seen.clone();
    let log = trace.clone();
    let reader = CommentReader::new(
        source(move |p, _| {
            let trace = log.clone();
            let slow = slow.clone();
            Box::pin(async move {
                let key = id(&p);
                trace
                    .lock()
                    .unwrap()
                    .push(J::object([("start", key.clone().into())]));
                match key.as_str() {
                    "root" => Ok(saved(&p, page(vec![index("slow", 1), index("failed", 1)]))),
                    "slow" => {
                        let wait = slow.lock().unwrap().take().unwrap();
                        wait.await.unwrap();
                        trace
                            .lock()
                            .unwrap()
                            .push(J::object([("finish", "slow".into())]));
                        Ok(saved(&p, leaf(1)))
                    }
                    _ => Err(raw(J::object([
                        ("name", "Error".into()),
                        ("message", "peer failed".into()),
                    ]))),
                }
            })
        }),
        tokio::runtime::Handle::current(),
    )
    .with_node_observer(Arc::new(move |p, _| {
        if id(p) == "slow" {
            observed.fetch_add(1, Ordering::SeqCst);
            after.notify_one();
        }
        Ok(())
    }));
    let error = match reader
        .range(&tree("root", 2), &task(), None, None, None)
        .await
    {
        Err(CommentError::Source(e)) => e,
        _ => panic!("peer failure missing"),
    };
    assert_eq!(error.value.get("message"), Some(&J::from("peer failed")));
    assert_eq!(seen.load(Ordering::SeqCst), 0);
    trace.lock().unwrap().push(J::object([
        ("observed", "peer failed".into()),
        ("slowCachedBefore", false.into()),
    ]));
    release.send(()).unwrap();
    completed.notified().await;
    ok(reader.node(&pin("slow"), &task(), None).await);
    trace
        .lock()
        .unwrap()
        .push(J::object([("slowCachedAfter", true.into())]));
    Ok(true.into())
}

#[tokio::test]
async fn retained_original_cases_match_at_the_agreed_public_boundary() {
    let fixture = fixture();
    assert_eq!(
        fixture.get("sourceHash"),
        Some(&digest(include_bytes!("../fixtures/sdk-comment-reader.ts.txt")).into())
    );
    let rows = fixture.get("cases").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 33);
    let mut observations = Vec::new();
    for row in rows {
        let name = row.get("name").unwrap().as_str().unwrap();
        if name == "negative start" {
            continue;
        }
        let trace = Arc::new(Mutex::new(Vec::new()));
        let result = observed(&name, trace.clone()).await;
        let actual = match result {
            Ok(value) => J::object([("value", value)]),
            Err(e) => J::object([("error", failure(e))]),
        };
        let expected = if let Some(value) = row.get("value") {
            let mut value = value.clone();
            if value.get("node").is_some() {
                value.remove("cacheBytes");
                value.remove("cacheSize");
            }
            J::object([("value", value)])
        } else {
            J::object([("error", row.get("error").unwrap().clone())])
        };
        assert_eq!(actual, expected, "{name}");
        let private_cache = name.starts_with("remember is")
            || name.starts_with("oversized new")
            || name.starts_with("cache bytes");
        if !private_cache {
            assert_eq!(
                J::Array(trace.lock().unwrap().clone()),
                row.get("trace").unwrap().clone(),
                "{name} trace"
            );
        }
        observations.push(J::object([
            ("name", name.into()),
            ("actual", actual),
            ("matches", true.into()),
            ("publicTrace", J::Array(trace.lock().unwrap().clone())),
            ("privateCacheProjectionExcluded", private_cache.into()),
        ]));
    }
    assert_eq!(observations.len(), 32);
    evidence(
        "comment-reader-retained-native.json",
        &J::Array(observations),
    );
}

#[tokio::test]
async fn observer_success_failure_hits_clones_ranges_and_remember_match_original_wrapper() {
    let original = std::process::Command::new("node")
        .args([
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/sdk-comment-reader-observer-oracle.mjs",
        ])
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        original.status.success(),
        "{}",
        String::from_utf8_lossy(&original.stderr)
    );
    let original = json(&original.stdout);
    evidence("comment-reader-observer-original.json", &original);
    let mut observations = Vec::new();
    for expected in original.get("cases").unwrap().as_array().unwrap() {
        let mode = expected.get("name").unwrap().as_str().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let events = Arc::new(Mutex::new(Vec::new()));
        let log = events.clone();
        let failing = mode.contains("failure");
        let fail = Arc::new(AtomicUsize::new(usize::from(failing)));
        let failures = fail.clone();
        let identity = Arc::new(());
        let marker = identity.clone();
        let is_range = mode.contains("range");
        let bad_identity = mode.contains("identity");
        let reader = CommentReader::new(
            source(move |p, _| {
                count.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    let mut value = if id(&p) == "root" {
                        page(vec![index("c1", 1), index("c2", 1)])
                    } else {
                        leaf(if id(&p) == "c2" { 2 } else { 1 })
                    };
                    if bad_identity && id(&p) == "c1" {
                        value.insert("taskId", "other".into());
                    }
                    Ok(saved(&p, value))
                })
            }),
            tokio::runtime::Handle::current(),
        )
        .with_node_observer(Arc::new(move |p, node| {
            log.lock().unwrap().push(J::object([
                ("pin", p.clone()),
                ("kind", node.get("kind").unwrap().clone()),
            ]));
            if (!is_range || id(p) == "c1") && failures.swap(0, Ordering::SeqCst) > 0 {
                return Err(Raw {
                    value: "observer raw".into(),
                    identity: marker.clone(),
                });
            }
            Ok(())
        }));
        if mode.contains("remember") {
            ok(reader.remember(&pin("c1"), Arc::new(leaf(1))));
            assert!(events.lock().unwrap().is_empty());
        }
        if mode.contains("range") {
            let result = reader
                .range(&tree("root", 2), &task(), None, None, None)
                .await;
            if failing {
                match result {
                    Err(CommentError::Source(e)) => assert!(Arc::ptr_eq(&identity, &e.identity)),
                    _ => panic!("range observer failure lost"),
                };
                for key in ["c1", "c2"] {
                    ok(reader.node(&pin(key), &task(), None).await);
                }
            } else if bad_identity {
                match result {
                    Err(CommentError::JavaScript(e)) => assert_eq!(
                        e.message,
                        JsString::from("The conversation snapshot belongs to another ticket.")
                    ),
                    _ => panic!("base identity failure lost"),
                };
                ok(reader.node(&pin("c2"), &task(), None).await);
            } else {
                ok(result);
            }
        } else {
            let first = reader.node(&pin("c1"), &task(), None).await;
            if failing {
                match first {
                    Err(CommentError::Source(e)) => {
                        assert!(Arc::ptr_eq(&identity, &e.identity));
                        assert_eq!(e.value, J::from("observer raw"));
                    }
                    _ => panic!("observer source identity lost"),
                }
            } else {
                ok(first);
            }
            if mode.contains("hit") || mode.contains("failure") || mode.contains("clone") {
                ok(reader.clone().node(&pin("c1"), &task(), None).await);
            }
        }
        let actual = J::object([
            ("name", mode.into()),
            ("events", J::Array(events.lock().unwrap().clone())),
            ("reads", J::Number(calls.load(Ordering::SeqCst) as f64)),
        ]);
        assert_eq!(&actual, expected);
        observations.push(actual);
    }
    evidence(
        "comment-reader-observer-native.json",
        &J::Array(observations),
    );
}

#[tokio::test]
async fn production_rpc_adapter_preserves_pin_cancellation_defaults_and_typed_failure() {
    struct Client {
        signal: CancellationToken,
        seen: Arc<AtomicUsize>,
    }
    impl RpcClient for Client {
        fn request<'a>(
            &'a self,
            method: &'a str,
            p: J,
            options: RequestOptions,
        ) -> RequestFuture<'a> {
            Box::pin(async move {
                assert_eq!(method, "objects.read");
                assert_eq!(p, pin("rpc"));
                assert_eq!(options.timeout, std::time::Duration::from_secs(35));
                assert!(options.bound_read.is_none());
                options.cancellation.cancel();
                assert!(self.signal.is_cancelled());
                self.seen.fetch_add(1, Ordering::SeqCst);
                Err(
                    ivy_sdk::IvyError::new_js(JsString(vec![0xd800]), JsString(vec![0xdc00]))
                        .with_outcome(ivy_sdk::Outcome::Completed)
                        .with_details(J::Array(vec!["kept".into()])),
                )
            })
        }
    }
    let signal = CancellationToken::new();
    let seen = Arc::new(AtomicUsize::new(0));
    let reader = CommentReader::new(
        Arc::new(RpcCommentSource::new(Arc::new(Client {
            signal: signal.clone(),
            seen: seen.clone(),
        }))),
        tokio::runtime::Handle::current(),
    );
    match reader.node(&pin("rpc"), &task(), Some(&signal)).await {
        Err(CommentError::Source(e)) => {
            assert_eq!(e.code_js(), JsString(vec![0xd800]));
            assert_eq!(e.message_js(), JsString(vec![0xdc00]));
            assert_eq!(e.outcome, ivy_sdk::Outcome::Completed);
            assert_eq!(e.details, Some(J::Array(vec!["kept".into()])));
        }
        _ => panic!("RPC failure changed"),
    };
    assert_eq!(seen.load(Ordering::SeqCst), 1);
}
