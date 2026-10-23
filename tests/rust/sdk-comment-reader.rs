use ivy_contracts::{
    IvyJson as J, JsString,
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

#[derive(Debug)]
struct ReadError(&'static str);
type Read =
    dyn Fn(J, Option<CancellationToken>) -> CommentReadFuture<'static, ReadError> + Send + Sync;
struct Source(Arc<Read>);
impl CommentSource for Source {
    type Error = ReadError;
    fn read<'a>(
        &'a self,
        pin: J,
        signal: Option<CancellationToken>,
    ) -> CommentReadFuture<'a, ReadError> {
        (self.0)(pin, signal)
    }
}
fn source(
    f: impl Fn(J, Option<CancellationToken>) -> CommentReadFuture<'static, ReadError>
    + Send
    + Sync
    + 'static,
) -> Arc<Source> {
    Arc::new(Source(Arc::new(f)))
}
fn json(text: &str) -> J {
    parse(text, usize::MAX).unwrap()
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
                ("contractVersion", "2.0.0".into()),
            ]),
        ),
        (
            "content",
            J::object([("encoding", "json".into()), ("value", value)]),
        ),
    ])
}
fn id(pin: &J) -> String {
    pin.get("objectId").unwrap().as_str().unwrap()
}
fn mutate<'a>(value: &'a mut J, path: &[&str]) -> &'a mut J {
    let Some((first, rest)) = path.split_first() else {
        return value;
    };
    let J::Object(fields) = value else {
        panic!("object required")
    };
    let value = &mut fields
        .iter_mut()
        .find(|(key, _)| key == &JsString::from(*first))
        .unwrap()
        .1;
    mutate(value, rest)
}
fn invalid<T: std::fmt::Debug>(result: Result<T, CommentError<ReadError>>, message: &str) {
    match result {
        Err(CommentError::Invalid(error)) => {
            assert_eq!(error.code, "internal_error");
            assert_eq!(error.message, message);
        }
        other => panic!("expected validation error: {other:?}"),
    }
}
const SNAPSHOT: &str = "The conversation snapshot belongs to another ticket.";

#[tokio::test]
async fn immutable_snapshots_check_contract_revision_and_each_owner_for_both_read_versions() {
    for version in ["1.0.0", "2.0.0"] {
        for case in 0..11 {
            let p = pin("n");
            let mut response = saved(&p, leaf(1));
            *mutate(&mut response, &["revision", "contractVersion"]) = version.into();
            match case {
                1 => *mutate(&mut response, &["object", "contractKey"]) = "other".into(),
                2 => *mutate(&mut response, &["revision", "contractVersion"]) = "3.0.0".into(),
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
                _ => {}
            }
            let calls = Arc::new(AtomicUsize::new(0));
            let reads = calls.clone();
            let reader = CommentReader::new(
                source(move |_, _| {
                    reads.fetch_add(1, Ordering::SeqCst);
                    let response = response.clone();
                    Box::pin(async move { Ok(response) })
                }),
                tokio::runtime::Handle::current(),
            );
            let result = reader.node(&p, &"task".into(), None).await;
            if case == 0 {
                let node = result.unwrap();
                let cancelled = CancellationToken::new();
                cancelled.cancel();
                assert!(Arc::ptr_eq(
                    &node,
                    &reader
                        .clone()
                        .node(&p, &"task".into(), Some(&cancelled))
                        .await
                        .unwrap()
                ));
                invalid(reader.node(&p, &"other".into(), None).await, SNAPSHOT);
                assert_eq!(calls.load(Ordering::SeqCst), 1);
            } else {
                invalid(result, SNAPSHOT);
            }
        }
    }
}

#[tokio::test]
async fn owner_and_pin_fields_are_typed_values_with_lossless_identifier_comparison() {
    let reader = CommentReader::new(
        source(|_, _| Box::pin(async { panic!("invalid pin read") })),
        tokio::runtime::Handle::current(),
    );
    for p in [
        json(r#"{"objectId":{},"revision":1}"#),
        json(r#"{"objectId":"n","revision":"1"}"#),
        json(r#"{"objectId":"n","revision":0}"#),
        json(r#"{"objectId":"","revision":1}"#),
    ] {
        invalid(
            reader.node(&p, &"task".into(), None).await,
            "The conversation index is invalid.",
        );
    }
    let owner = JsString(vec![0xd800, 0x41]);
    let requested = owner.clone();
    let reader = CommentReader::new(
        source(move |p, _| {
            let mut response = saved(&p, leaf(1));
            for path in [
                &["object", "parentId"][..],
                &["object", "ownerObjectId"],
                &["content", "value", "taskId"],
            ] {
                *mutate(&mut response, path) = J::String(owner.clone());
            }
            Box::pin(async move { Ok(response) })
        }),
        tokio::runtime::Handle::current(),
    );
    reader.node(&pin("n"), &requested, None).await.unwrap();
    invalid(
        reader
            .node(&pin("n"), &JsString(vec![0xfffd, 0x41]), None)
            .await,
        SNAPSHOT,
    );
}

#[tokio::test]
async fn ranges_check_bounds_sequences_child_sums_and_depth_before_reading_invalid_indices() {
    let empty = json(r#"{"root":null,"length":0}"#);
    let no_reads = CommentReader::new(
        source(|_, _| Box::pin(async { panic!("invalid range read") })),
        tokio::runtime::Handle::current(),
    );
    assert!(
        no_reads
            .range(&empty, &"task".into(), None, None, None)
            .await
            .unwrap()
            .is_empty()
    );
    let mut mismatch = tree("root", 2);
    *mutate(&mut mismatch, &["root", "length"]) = J::Number(1.0);
    for (t, start, end) in [
        (tree("root", 2), Some(0), Some(3)),
        (tree("root", 2), Some(2), Some(1)),
        (mismatch, None, None),
    ] {
        invalid(
            no_reads.range(&t, &"task".into(), start, end, None).await,
            "The conversation range is invalid.",
        );
    }
    for (root, length, message) in [
        (leaf(2), 1, "The conversation sequence is invalid."),
        (leaf(1), 2, "The conversation sequence is invalid."),
        (page(vec![]), 1, "The conversation index is incomplete."),
        (
            page(vec![index("leaf", 1)]),
            2,
            "The conversation index is incomplete.",
        ),
        (
            page(vec![json(
                r#"{"object":{"objectId":"leaf","revision":1},"length":"1"}"#,
            )]),
            1,
            "The conversation index is incomplete.",
        ),
        (
            J::object([("taskId", "task".into()), ("kind", "page".into())]),
            1,
            "The conversation index is incomplete.",
        ),
    ] {
        let reader = CommentReader::new(
            source(move |p, _| {
                let root = root.clone();
                Box::pin(async move { Ok(saved(&p, root)) })
            }),
            tokio::runtime::Handle::current(),
        );
        invalid(
            reader
                .range(&tree("root", length), &"task".into(), None, None, None)
                .await,
            message,
        );
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let reader = CommentReader::new(
        source(move |p, _| {
            let depth = id(&p)[1..].parse::<usize>().unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(saved(&p, page(vec![index(&format!("d{}", depth + 1), 1)]))) })
        }),
        tokio::runtime::Handle::current(),
    );
    invalid(
        reader
            .range(&tree("d0", 1), &"task".into(), None, None, None)
            .await,
        "The conversation index is invalid.",
    );
    assert_eq!(calls.load(Ordering::SeqCst), 13);
}

#[tokio::test]
async fn range_visits_intersections_and_returns_independent_lossless_comment_entries() {
    let mut c3 = leaf(3);
    *mutate(&mut c3, &["entry", "comment", "body"]) =
        J::String(JsString(vec![0xd800, 0xd83d, 0xde00]));
    mutate(&mut c3, &["entry", "comment"]).insert("unknown", J::Number(17.0));
    let expected = c3.get("entry").unwrap().clone();
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
    ]);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let log = calls.clone();
    let reader = CommentReader::new(
        source(move |p, _| {
            let key = id(&p);
            log.lock().unwrap().push(key.clone());
            let value = docs[&key].clone();
            Box::pin(async move { Ok(saved(&p, value)) })
        }),
        tokio::runtime::Handle::current(),
    );
    let mut result = reader
        .range(&tree("root", 4), &"task".into(), Some(2), Some(3), None)
        .await
        .unwrap();
    assert_eq!(result, vec![expected.clone()]);
    *mutate(&mut result[0], &["comment", "body"]) = "changed".into();
    assert_eq!(
        reader
            .range(&tree("root", 4), &"task".into(), Some(2), Some(3), None)
            .await
            .unwrap(),
        vec![expected]
    );
    assert_eq!(*calls.lock().unwrap(), vec!["root", "right", "c3"]);
}

#[tokio::test]
async fn cache_is_bounded_first_write_wins_and_versions_are_independent() {
    let mut node = leaf(1);
    *mutate(&mut node, &["entry", "comment", "body"]) = "😀".into();
    let size = stringify(&node, usize::MAX).unwrap().len();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let log = calls.clone();
    let reader = CommentReader::with_maximum_bytes(
        source(move |p, _| {
            log.lock().unwrap().push(p.clone());
            Box::pin(async move { Ok(saved(&p, leaf(1))) })
        }),
        tokio::runtime::Handle::current(),
        size * 2,
    );
    let first = Arc::new(node);
    reader.remember(&pin("a"), first.clone()).unwrap();
    reader.remember(&pin("b"), first.clone()).unwrap();
    reader.remember(&pin("a"), Arc::new(leaf(2))).unwrap();
    assert!(Arc::ptr_eq(
        &reader.node(&pin("a"), &"task".into(), None).await.unwrap(),
        &first
    ));
    let mut huge = leaf(1);
    *mutate(&mut huge, &["entry", "comment", "body"]) = "x".repeat(size * 3).into();
    reader.remember(&pin("huge"), Arc::new(huge)).unwrap();
    reader.node(&pin("b"), &"task".into(), None).await.unwrap();
    assert!(calls.lock().unwrap().is_empty());
    reader.remember(&pin("c"), first).unwrap();
    reader.node(&pin("a"), &"task".into(), None).await.unwrap();
    let mut next = pin("a");
    next.insert("revision", J::Number(2.0));
    reader.node(&next, &"task".into(), None).await.unwrap();
    assert_eq!(*calls.lock().unwrap(), vec![pin("a"), next]);
}

#[tokio::test]
async fn concurrent_batches_are_bounded_and_return_sequence_order() {
    type Gate = (oneshot::Sender<()>, oneshot::Receiver<()>);
    let waiting = Arc::new(Mutex::new(Vec::<Gate>::new()));
    let notify = Arc::new(Notify::new());
    let count = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let (w, n) = (waiting.clone(), notify.clone());
    let driver = tokio::spawn(async move {
        for size in [8, 8, 1] {
            let wave = loop {
                let changed = n.notified();
                if w.lock().unwrap().len() == size {
                    break std::mem::take(&mut *w.lock().unwrap());
                }
                changed.await;
            };
            for (send, ack) in wave.into_iter().rev() {
                send.send(()).unwrap();
                ack.await.unwrap();
            }
        }
    });
    let peak = maximum.clone();
    let reader = CommentReader::new(
        source(move |p, _| {
            let (waiting, notify, count, maximum) = (
                waiting.clone(),
                notify.clone(),
                count.clone(),
                maximum.clone(),
            );
            Box::pin(async move {
                let key = id(&p);
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
                count.fetch_sub(1, Ordering::SeqCst);
                ack.send(()).unwrap();
                Ok(saved(&p, leaf(key[1..].parse().unwrap())))
            })
        }),
        tokio::runtime::Handle::current(),
    );
    let entries = reader
        .range(&tree("root", 17), &"task".into(), None, None, None)
        .await
        .unwrap();
    driver.await.unwrap();
    assert_eq!(peak.load(Ordering::SeqCst), 8);
    for (position, entry) in entries.iter().enumerate() {
        assert_eq!(
            entry.get("comment").unwrap().get("sequence"),
            Some(&J::Number((position + 1) as f64))
        );
    }
}

#[tokio::test]
async fn failed_batch_keeps_other_started_immutable_reads_available() {
    let (first_release, first_slow) = oneshot::channel();
    let (last_release, last_slow) = oneshot::channel();
    let slow = Arc::new(Mutex::new(vec![last_slow, first_slow]));
    let completed = Arc::new(Notify::new());
    let done = completed.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    let reader = CommentReader::new(
        source(move |p, _| {
            let slow = slow.clone();
            let count = count.clone();
            Box::pin(async move {
                match id(&p).as_str() {
                    "root" => Ok(saved(&p, page(vec![index("slow", 1), index("failed", 1)]))),
                    "slow" => {
                        count.fetch_add(1, Ordering::SeqCst);
                        let wait = slow.lock().unwrap().pop().unwrap();
                        wait.await.unwrap();
                        Ok(saved(&p, leaf(1)))
                    }
                    _ => Err(ReadError("peer failed")),
                }
            })
        }),
        tokio::runtime::Handle::current(),
    )
    .with_node_observer(Arc::new(move |p, _| {
        if id(p) == "slow" {
            done.notify_one();
        }
        Ok(())
    }));
    let clone = reader.clone();
    for caller in [&reader, &clone] {
        match caller
            .range(&tree("root", 2), &"task".into(), None, None, None)
            .await
        {
            Err(CommentError::Source(ReadError("peer failed"))) => {}
            other => panic!("{other:?}"),
        }
    }
    let mut settlement = Box::pin(clone.settled());
    assert!(futures_util::poll!(&mut settlement).is_pending());
    first_release.send(()).unwrap();
    completed.notified().await;
    assert!(futures_util::poll!(&mut settlement).is_pending());
    last_release.send(()).unwrap();
    settlement.await;
    reader
        .node(&pin("slow"), &"task".into(), None)
        .await
        .unwrap();
    assert_eq!(reads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn settlement_keeps_direct_reads_and_abandoned_range_work_owned() {
    for direct in [true, false] {
        let (started, entered) = oneshot::channel();
        let (release, held) = oneshot::channel();
        let state = Arc::new(Mutex::new(Some((started, held))));
        let reads = Arc::new(AtomicUsize::new(0));
        let count = reads.clone();
        let reader = CommentReader::new(
            source(move |p, _| {
                count.fetch_add(1, Ordering::SeqCst);
                let (started, held) = state.lock().unwrap().take().unwrap();
                Box::pin(async move {
                    started.send(()).unwrap();
                    held.await.unwrap();
                    Ok(saved(&p, leaf(1)))
                })
            }),
            tokio::runtime::Handle::current(),
        );
        let caller = reader.clone();
        let call = tokio::spawn(async move {
            if direct {
                caller.node(&pin("held"), &"task".into(), None).await?;
            } else {
                caller
                    .range(&tree("held", 1), &"task".into(), None, None, None)
                    .await?;
            }
            Ok::<_, CommentError<ReadError>>(())
        });
        entered.await.unwrap();
        if !direct {
            call.abort();
        }
        let mut settlement = Box::pin(reader.settled());
        assert!(futures_util::poll!(&mut settlement).is_pending());
        release.send(()).unwrap();
        settlement.await;
        let completed = call.await;
        if direct {
            completed.unwrap().unwrap();
        } else {
            assert!(completed.unwrap_err().is_cancelled());
        }
        reader
            .node(&pin("held"), &"task".into(), None)
            .await
            .unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn observers_run_after_owner_validation_and_preserve_typed_failures() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let reader = CommentReader::new(
        source(|_, _| Box::pin(async { panic!("remembered node read") })),
        tokio::runtime::Handle::current(),
    )
    .with_node_observer(Arc::new(move |_, _| {
        if count.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(ReadError("observer failed"))
        } else {
            Ok(())
        }
    }));
    reader.remember(&pin("c1"), Arc::new(leaf(1))).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    invalid(
        reader.node(&pin("c1"), &"other".into(), None).await,
        SNAPSHOT,
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    match reader.node(&pin("c1"), &"task".into(), None).await {
        Err(CommentError::Source(ReadError("observer failed"))) => {}
        other => panic!("{other:?}"),
    }
    reader
        .clone()
        .node(&pin("c1"), &"task".into(), None)
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn rpc_adapter_keeps_pin_cancellation_defaults_and_lossless_error_fields() {
    struct Client {
        signal: CancellationToken,
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
                Err(
                    ivy_sdk::IvyError::new_js(JsString(vec![0xd800]), JsString(vec![0xdc00]))
                        .with_outcome(ivy_sdk::Outcome::Completed)
                        .with_details(J::Array(vec!["kept".into()])),
                )
            })
        }
    }
    let signal = CancellationToken::new();
    let reader = CommentReader::new(
        Arc::new(RpcCommentSource::new(Arc::new(Client {
            signal: signal.clone(),
        }))),
        tokio::runtime::Handle::current(),
    );
    match reader
        .node(&pin("rpc"), &"task".into(), Some(&signal))
        .await
    {
        Err(CommentError::Source(error)) => {
            assert_eq!(error.code_js(), JsString(vec![0xd800]));
            assert_eq!(error.message_js(), JsString(vec![0xdc00]));
            assert_eq!(error.outcome, ivy_sdk::Outcome::Completed);
            assert_eq!(error.details, Some(J::Array(vec!["kept".into()])));
        }
        other => panic!("{other:?}"),
    }
}
