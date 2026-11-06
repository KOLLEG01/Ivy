use ivy_agent_manager::{notice_policy::NativeNoticePolicy, project_model::NativeProjects};
use ivy_contracts::{
    IvyJson, IvyResult, JsString,
    json::{parse, stringify},
};
use ivy_host_runtime::codex_rpc::{Events, NativeRpc};
use std::{
    future::{Future, poll_fn},
    sync::{Arc, Mutex},
    task::Poll,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    task::JoinHandle,
};

fn empty() -> IvyJson {
    IvyJson::object([] as [(&str, IvyJson); 0])
}
fn policy() -> NativeNoticePolicy {
    let mut policy = NativeNoticePolicy::default();
    policy.set_projects(NativeProjects::from_summaries(&[super::project("internal")]).unwrap());
    policy
}
fn internal() -> IvyJson {
    IvyJson::object([("projectId", "internal".into())])
}
fn known(policy: &NativeNoticePolicy, id: &str) -> Option<bool> {
    policy.is_internal(&id.into()).unwrap()
}

#[test]
fn count_eviction_and_older_bulk_leave_newer_membership_unknown() {
    let mut policy = policy();
    policy.set_thread("old".into(), &internal(), None);
    for i in 0..10_000 {
        policy.set_thread(format!("t{i}").into(), &internal(), None);
        assert!(policy.retained_bytes() <= 2 * 1024 * 1024);
    }
    assert_eq!(known(&policy, "old"), None);
    assert_eq!(known(&policy, "t8975"), None);
    assert!((8976..10_000).all(|i| known(&policy, &format!("t{i}")) == Some(true)));
    policy.set_thread("old".into(), &empty(), Some(0.0));
    assert_eq!(known(&policy, "old"), None);
    policy
        .set_threads(
            &[IvyJson::object([
                ("nativeId", "old".into()),
                ("summary", empty()),
            ])],
            0.0,
        )
        .unwrap();
    assert_eq!(known(&policy, "old"), None);
    assert_eq!(known(&policy, "t9999"), Some(true));
    // A fresh observation can restore an evicted row at the current revision.
    policy.set_thread("old".into(), &internal(), Some(policy.revision()));
    assert_eq!(known(&policy, "old"), Some(true));
}

#[test]
fn heap_eviction_and_oversized_pushes_do_not_leave_old_external_membership() {
    let mut policy = policy();
    let large = IvyJson::object([
        ("cwd", "p".repeat(32 * 1024).into()),
        ("projectId", "internal".into()),
    ]);
    for i in 0..200 {
        policy.set_thread(format!("t{i}").into(), &large, None);
        assert!(policy.retained_bytes() <= 2 * 1024 * 1024);
    }
    let retained = (0..200)
        .filter(|i| known(&policy, &format!("t{i}")).is_some())
        .count();
    assert!(
        (2..40).contains(&retained),
        "Heap, rather than count, must limit large memberships"
    );
    assert_eq!(known(&policy, "t0"), None);
    assert_eq!(known(&policy, "t199"), Some(true));
    policy.set_thread("large".into(), &empty(), None);
    assert_eq!(known(&policy, "large"), Some(false));
    let oversized = IvyJson::object([("cwd", "x".repeat(256 * 1024).into())]);
    policy.set_thread("large".into(), &oversized, None);
    assert_eq!(known(&policy, "large"), None);
    policy.set_thread("large".into(), &empty(), Some(0.0));
    assert_eq!(known(&policy, "large"), None);
    policy.set_thread("project".into(), &empty(), None);
    policy.observe(
        &"thread/project/updated".into(),
        &IvyJson::object([
            ("threadId", "project".into()),
            ("projectId", "p".repeat(256 * 1024).into()),
        ]),
    );
    assert_eq!(known(&policy, "project"), None);
    policy.set_threads(&[], policy.revision()).unwrap();
    assert!(
        policy.retained_bytes() < 4096,
        "Empty cache releases its index allocation"
    );
}

struct Native {
    policy: Arc<Mutex<NativeNoticePolicy>>,
    rpc: NativeRpc,
    requests: tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
    replies: tokio::io::DuplexStream,
}
impl Native {
    fn new() -> Self {
        Self::with_catalog(include_str!(
            "../../../specs/native/codex-0.159.2/catalog.json"
        ))
    }
    fn with_catalog(source: &str) -> Self {
        let catalog = Arc::new(parse(source, 4 * 1024 * 1024).unwrap());
        let (input, requests) = tokio::io::duplex(65536);
        let (replies, output) = tokio::io::duplex(65536);
        let policy = Arc::new(Mutex::new(policy()));
        let events = Events {
            on_notification: Arc::new({
                let policy = policy.clone();
                move |event| {
                    policy.lock().unwrap().observe(&event.method, &event.params);
                    Box::pin(async { Ok(()) })
                }
            }),
            ..Events::default()
        };
        Self {
            policy,
            rpc: NativeRpc::start(catalog, input, output, events).unwrap(),
            requests: BufReader::new(requests).lines(),
            replies,
        }
    }
    fn read(&self, id: &str) -> JoinHandle<IvyResult<()>> {
        let policy = self.policy.clone();
        let rpc = self.rpc.clone();
        let id = JsString::from(id);
        tokio::spawn(async move { NativeNoticePolicy::ensure(&policy, &rpc, id).await })
    }
    async fn request(&mut self, id: &str) -> IvyJson {
        let line = tokio::time::timeout(Duration::from_secs(5), self.requests.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let request = parse(&line, 65536).unwrap();
        assert_eq!(request.get("method"), Some(&"thread/read".into()));
        assert_eq!(
            request.get("params"),
            Some(&IvyJson::object([
                ("threadId", id.into()),
                ("includeTurns", false.into()),
            ]))
        );
        request
    }
    async fn reply(&mut self, request: &IvyJson, id: &str, internal: bool) {
        let thread = IvyJson::object([
            ("id", id.into()),
            (
                "projectId",
                if internal {
                    "internal".into()
                } else {
                    IvyJson::Null
                },
            ),
        ]);
        let reply = IvyJson::object([
            ("id", request.required("id").unwrap().clone()),
            ("result", IvyJson::object([("thread", thread)])),
        ]);
        self.replies
            .write_all(format!("{}\n", stringify(&reply, 65536).unwrap()).as_bytes())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn reads_coalesce_at_capacity_and_outlive_a_cancelled_waiter() {
    let mut native = Native::new();
    let mut reads = Vec::new();
    for i in 0..8 {
        let id = format!("t{i}");
        let read = native.read(&id);
        let request = native.request(&id).await;
        reads.push((read, request));
    }
    assert_eq!(
        native.read("overflow").await.unwrap().unwrap_err().code,
        "native_inventory_capacity"
    );
    let policy = native.policy.clone();
    let rpc = native.rpc.clone();
    let mut joined = Box::pin(NativeNoticePolicy::ensure(&policy, &rpc, "t0".into()));
    poll_fn(|cx| match joined.as_mut().poll(cx) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(_) => panic!("Coalesced read must await the original response"),
    })
    .await;
    let (cancelled, first) = reads.remove(0);
    cancelled.abort();
    assert!(cancelled.await.unwrap_err().is_cancelled());
    assert_eq!(
        native.read("overflow").await.unwrap().unwrap_err().code,
        "native_inventory_capacity"
    );
    native.reply(&first, "t0", true).await;
    joined.await.unwrap();
    assert_eq!(known(&policy.lock().unwrap(), "t0"), Some(true));
    let next = native.read("replacement");
    let request = native.request("replacement").await;
    native.reply(&request, "replacement", false).await;
    next.await.unwrap().unwrap();
    // Repeated known membership is not another native request.
    native.read("replacement").await.unwrap().unwrap();
    for (i, (read, request)) in reads.into_iter().enumerate() {
        native.reply(&request, &format!("t{}", i + 1), true).await;
        read.await.unwrap().unwrap();
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(30), native.requests.next_line())
            .await
            .is_err()
    );
    native.rpc.close("fixture_done").await;
}

#[tokio::test]
async fn wrong_identity_fails_without_a_cache_entry_and_releases_the_slot() {
    let mut native = Native::new();
    let read = native.read("original");
    let request = native.request("original").await;
    native.reply(&request, "foreign", false).await;
    assert_eq!(
        read.await.unwrap().unwrap_err().code,
        "native_inventory_unavailable"
    );
    assert_eq!(known(&native.policy.lock().unwrap(), "original"), None);
    assert_eq!(known(&native.policy.lock().unwrap(), "foreign"), None);
    let read = native.read("original");
    let request = native.request("original").await;
    native.reply(&request, "original", true).await;
    read.await.unwrap().unwrap();
    assert_eq!(
        known(&native.policy.lock().unwrap(), "original"),
        Some(true)
    );
    assert_eq!(
        native
            .read(&"x".repeat(257))
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "native_inventory_capacity"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), native.requests.next_line())
            .await
            .is_err()
    );
    native.rpc.close("fixture_done").await;
}

#[tokio::test]
async fn reply_older_than_an_evicted_push_cannot_mark_an_internal_thread_external() {
    let mut native = Native::new();
    let read = native.read("original");
    let request = native.request("original").await;
    {
        let mut policy = native.policy.lock().unwrap();
        policy.set_thread("original".into(), &internal(), None);
        for i in 0..1024 {
            policy.set_thread(format!("other{i}").into(), &internal(), None);
        }
        assert_eq!(known(&policy, "original"), None);
    }
    native.reply(&request, "original", false).await;
    read.await.unwrap().unwrap();
    assert_eq!(known(&native.policy.lock().unwrap(), "original"), None);
    let read = native.read("original");
    let request = native.request("original").await;
    native.reply(&request, "original", true).await;
    read.await.unwrap().unwrap();
    assert_eq!(
        known(&native.policy.lock().unwrap(), "original"),
        Some(true)
    );
    native.rpc.close("fixture_done").await;
}

#[tokio::test]
async fn deletion_on_the_native_wire_fences_older_reads_and_bulk_membership() {
    for (source, internal_reply) in [
        (
            include_str!("../../../specs/native/codex-0.159.2/catalog.json"),
            false,
        ),
        (
            include_str!("../../../specs/native/codex-0.163.0/catalog.json"),
            true,
        ),
    ] {
        let mut native = Native::with_catalog(source);
        let revision = native.policy.lock().unwrap().revision();
        let read = native.read("original");
        let request = native.request("original").await;
        let deleted = IvyJson::object([
            ("method", "thread/deleted".into()),
            ("params", IvyJson::object([("threadId", "original".into())])),
        ]);
        native
            .replies
            .write_all(format!("{}\n", stringify(&deleted, 65536).unwrap()).as_bytes())
            .await
            .unwrap();
        native.reply(&request, "original", internal_reply).await;
        read.await.unwrap().unwrap();
        {
            let mut policy = native.policy.lock().unwrap();
            assert_eq!(
                known(&policy, "original"),
                None,
                "A read started before deletion resurrected its membership"
            );
            assert!(policy.revision() > revision);
            policy
                .set_threads(
                    &[IvyJson::object([
                        ("nativeId", "original".into()),
                        ("summary", internal()),
                    ])],
                    revision,
                )
                .unwrap();
            assert_eq!(known(&policy, "original"), None);
            // Deletions reuse the bounded discard fence, without per-ID tombstones.
            for i in 0..10_000 {
                policy.observe(
                    &"thread/deleted".into(),
                    &IvyJson::object([("threadId", format!("deleted{i}").into())]),
                );
            }
            assert!(policy.retained_bytes() < 4096);
        }
        // A new observation remains possible, and the old slot was released.
        let read = native.read("original");
        let request = native.request("original").await;
        native.reply(&request, "original", true).await;
        read.await.unwrap().unwrap();
        assert_eq!(
            known(&native.policy.lock().unwrap(), "original"),
            Some(true)
        );
        native.rpc.close("fixture_done").await;
    }
}

#[tokio::test]
async fn observation_deadline_releases_capacity_and_late_replies_stay_unknown() {
    let mut native = Native::new();
    let mut reads = Vec::new();
    let mut requests = Vec::new();
    for i in 0..8 {
        let id = format!("t{i}");
        reads.push(native.read(&id));
        requests.push(native.request(&id).await);
    }
    assert_eq!(
        native.read("overflow").await.unwrap().unwrap_err().code,
        "native_inventory_capacity"
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        for read in reads {
            assert_eq!(
                read.await.unwrap().unwrap_err().code,
                "native_observation_deadline"
            );
        }
    })
    .await
    .unwrap();
    assert!(
        native.rpc.connected(),
        "A metadata timeout cannot close active native work"
    );
    for (i, request) in requests.iter().enumerate() {
        native.reply(request, &format!("t{i}"), false).await;
    }
    let read = native.read("after-timeout");
    let request = native.request("after-timeout").await;
    native.reply(&request, "after-timeout", true).await;
    read.await.unwrap().unwrap();
    let policy = native.policy.lock().unwrap();
    assert!((0..8).all(|i| known(&policy, &format!("t{i}")).is_none()));
    assert_eq!(known(&policy, "after-timeout"), Some(true));
    drop(policy);
    native.rpc.close("fixture_done").await;
}

#[tokio::test]
async fn native_close_releases_reads_and_does_not_mix_separate_policy_owners() {
    let mut first = Native::new();
    let mut second = Native::new();
    let mut blocked = Vec::new();
    for i in 0..8 {
        let id = format!("t{i}");
        blocked.push(first.read(&id));
        first.request(&id).await;
    }
    let independent = second.read("t0");
    let request = second.request("t0").await;
    second.reply(&request, "t0", true).await;
    independent.await.unwrap().unwrap();
    assert_eq!(known(&first.policy.lock().unwrap(), "t0"), None);
    assert_eq!(known(&second.policy.lock().unwrap(), "t0"), Some(true));
    first.rpc.close("fixture_owner_closed").await;
    for read in blocked {
        assert!(
            tokio::time::timeout(Duration::from_secs(5), read)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }
    // Admission is free again; failure comes from the original closed RPC.
    assert_eq!(
        first.read("new").await.unwrap().unwrap_err().code,
        "native_unavailable"
    );
    assert!(second.rpc.connected());
    assert_eq!(known(&second.policy.lock().unwrap(), "t0"), Some(true));
    second.rpc.close("fixture_done").await;
}
