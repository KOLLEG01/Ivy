use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, json::canonical,
    limits::JSON_OBJECT_CONTENT_BYTES,
};
use ivy_task_board::{
    engine_actions::result_comment_fits,
    engine_read::{comment_reads::task_view_page, conversation::ConversationPolicies},
    journal_store::persistent_reads::task_comments::validate_task_content,
    operations::{Document, ObjectPin, Operations, Store},
    publication::Publication,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::{Cell, RefCell},
    future::Future,
    sync::atomic::{AtomicIsize, AtomicUsize, Ordering},
};

struct Counted;
#[path = "task-board-evidence-bytes.rs"]
mod evidence_bytes;
#[path = "task-board-upload-admission.rs"]
mod upload_admission;
#[test]
fn object_pin_moves_utf16_identity_and_preserves_numeric_wire_hash() {
    for revision in [1.0_f64, -0.0, 0.5, 9007199254740991.0] {
        let id = JsString(vec![0x61, 0xd800, 0x62, 0xdc00]);
        let pointer = id.0.as_ptr();
        let wire = Json::object([
            ("objectId", Json::String(id)),
            ("revision", Json::Number(revision)),
        ]);
        let expected = ivy_contracts::hash::hash_json(&wire).unwrap();
        let pin = ObjectPin::from_json(wire).unwrap();
        assert_eq!(pin.object_id.0.as_ptr(), pointer);
        assert_eq!(pin.revision.to_bits(), revision.to_bits());
        assert_eq!(
            ivy_contracts::hash::hash_json(&pin.to_json()).unwrap(),
            expected
        );
        let wire = pin.into_json();
        assert_eq!(
            wire.get("objectId")
                .unwrap()
                .as_js_string()
                .unwrap()
                .0
                .as_ptr(),
            pointer
        );
        assert_eq!(ivy_contracts::hash::hash_json(&wire).unwrap(), expected);
    }
}

static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
fn account(bytes: isize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}
unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            account(layout.size() as isize);
            ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(p, layout, size) };
        if !result.is_null() {
            account(size as isize - layout.size() as isize);
            ALLOCATED.fetch_add(size, Ordering::Relaxed);
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        result
    }
}
#[global_allocator]
static ALLOCATOR: Counted = Counted;
fn peak(action: impl FnOnce()) -> isize {
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    action();
    PEAK.load(Ordering::Relaxed) - before
}
fn allocations(action: impl FnOnce()) -> (isize, usize, usize, isize) {
    let live = LIVE.load(Ordering::Relaxed);
    let bytes = ALLOCATED.load(Ordering::Relaxed);
    let count = ALLOCATIONS.load(Ordering::Relaxed);
    let peak = peak(action);
    (
        peak,
        ALLOCATED.load(Ordering::Relaxed) - bytes,
        ALLOCATIONS.load(Ordering::Relaxed) - count,
        LIVE.load(Ordering::Relaxed) - live,
    )
}
fn isolated(name: &str) -> bool {
    const CHILD: &str = "IVY_TASK_COPY_CHILD";
    if std::env::var_os(CHILD).is_some() {
        return false;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("copy_projections::{name}"),
            "--test-threads=1",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("1 passed;"));
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
    true
}

#[test]
fn remote_record_moves_owned_payload_and_metadata_before_cache_admission() {
    if isolated("remote_record_moves_owned_payload_and_metadata_before_cache_admission") {
        return;
    }
    use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
    use ivy_task_board::{
        engine_read::read_policy::QuestionPolicies,
        journal_store::persistent_reads::{
            local_reads::SqliteReads,
            store::{ObjectReads, PersistentReadStore},
            task_comments::TaskComments,
        },
        journal_store::{Contracts, JournalStore, ReadCache, ReadScopes},
        local_journal::{LocalJournal, initialize},
        local_retention::HistoryLimits,
        work_budget::WorkBudget,
    };
    use std::sync::{Arc, Mutex};
    struct Catalog;
    impl Contracts for Catalog {
        fn version(&self, _: &str) -> IvyResult<String> {
            Ok("1.0.0".into())
        }
        fn local(&self, _: &str) -> IvyResult<bool> {
            Ok(false)
        }
        fn readable_version(&self, _: &str, version: &JsString) -> IvyResult<bool> {
            Ok(version == &JsString::from("1.0.0"))
        }
        fn validate_record(&self, _: &str, _: &JsString, _: &Json) -> IvyResult<()> {
            Ok(())
        }
        fn validate_definition(&self, _: &str, _: &Json) -> IvyResult<()> {
            unreachable!()
        }
    }
    struct Source(Mutex<Option<Json>>);
    impl RpcClient for Source {
        fn request<'a>(&'a self, method: &'a str, _: Json, _: RequestOptions) -> RequestFuture<'a> {
            Box::pin(async move {
                assert_eq!(method, "objects.read");
                Ok(self.0.lock().unwrap().take().expect("one original read"))
            })
        }
    }
    impl ObjectReads<IvyError> for Source {
        async fn read(&self, params: Option<Json>) -> IvyResult<Json> {
            self.request("objects.read", params.unwrap(), RequestOptions::default())
                .await
        }
    }
    struct Policies;
    impl QuestionPolicies<IvyError> for Policies {
        fn open_questions(&self, _: &Json) -> IvyResult<Vec<Json>> {
            unreachable!()
        }
    }
    struct NoComments;
    impl ivy_sdk::task_comments::CommentSource for NoComments {
        type Error = IvyError;
        fn read<'a>(
            &'a self,
            _: Json,
            _: Option<ivy_sdk::task_comments::CancellationToken>,
        ) -> ivy_sdk::task_comments::CommentReadFuture<'a, IvyError> {
            Box::pin(async { panic!("history reads do not hydrate comments") })
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(tokio::task::yield_now());
    let db = ivy_store::rusqlite::Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    // Exercise the raw workflow reader and its full Store::read materializer.
    for (key, workflow, cached) in ["task-board/history", "task-board/run", "task-board/result"]
        .into_iter()
        .flat_map(|key| {
            [
                (0, false),
                (0, true),
                (1, false),
                (1, true),
                (2, false),
                (2, true),
            ]
            .map(|(workflow, cached)| (key, workflow, cached))
        })
    {
        let payload = Json::Array(vec![Json::Null; 4096]);
        let value_pointer = payload.as_array().unwrap().as_ptr();
        let value = match key {
            "task-board/history" => payload,
            "task-board/run" => Json::object([
                ("opaque", payload),
                (
                    "executionContext",
                    Json::object([
                        ("latestResult", large_original()),
                        ("nextSteps", Json::Null),
                    ]),
                ),
            ]),
            "task-board/result" => Json::object([
                ("opaque", payload),
                (
                    "content",
                    Json::object([("summary", large_original()), ("checks", Json::Null)]),
                ),
            ]),
            _ => unreachable!(),
        };
        let text_pointer = match key {
            "task-board/run" => Some(
                f(f(&value, "executionContext"), "latestResult")
                    .as_js_string()
                    .unwrap()
                    .0
                    .as_ptr(),
            ),
            "task-board/result" => Some(
                f(f(&value, "content"), "summary")
                    .as_js_string()
                    .unwrap()
                    .0
                    .as_ptr(),
            ),
            _ => None,
        };
        let metadata = Json::object([
            ("id", "remote".into()),
            ("contractKey", key.into()),
            ("parentId", "root".into()),
            ("effectivelyArchived", false.into()),
            (
                "opaque",
                Json::String(JsString(vec![0xd800, 0xdc00].repeat(65536))),
            ),
        ]);
        let metadata_pointer = metadata.as_object().unwrap().as_ptr();
        let references = Json::Object(
            (0..1024)
                .map(|index| {
                    (
                        format!("reference-{index}").into(),
                        Json::object([
                            ("objectId", "00000000-0000-4000-8000-000000000001".into()),
                            ("revision", 1.0.into()),
                        ]),
                    )
                })
                .collect(),
        );
        let source = Arc::new(Source(Mutex::new(Some(Json::object([
            ("object", metadata),
            (
                "revision",
                Json::object([
                    ("revision", 7.0.into()),
                    ("contractVersion", "1.0.0".into()),
                    ("mediaType", "application/json".into()),
                    ("contentHash", "original".into()),
                    ("byteLength", 20481.0.into()),
                    ("references", references),
                ]),
            ),
            (
                "content",
                Json::object([("encoding", "json".into()), ("value", value)]),
            ),
        ])))));
        let cache = ReadCache::default();
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: source.clone(),
            contracts: Catalog,
            limits: HistoryLimits {
                count: 128,
                bytes: 16 * 1024 * 1024,
            },
            now: || "now".into(),
            new_id: || "unused".into(),
            cache: cached.then_some(&cache),
        };
        let local = SqliteReads {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            contracts: &Catalog,
            parse: |raw: &str| {
                ivy_contracts::json::parse(raw, ivy_contracts::limits::MANAGEMENT_FRAME_BYTES)
            },
            sqlite_failure: ivy_store::sqlite::error,
        };
        let comments = TaskComments::new(Arc::new(NoComments), runtime.handle().clone());
        let source_failure = |error| error;
        let reads: PersistentReadStore<'_, _, _, _, _, _, _, IvyError> = PersistentReadStore {
            local: &local,
            objects: source.as_ref(),
            policies: &Policies,
            comments: &comments,
            source_failure: &source_failure,
            cache: cached.then_some(&cache),
        };
        let mut output = None;
        let allocated = peak(|| {
            let pin = Json::object([("objectId", "remote".into()), ("revision", 7.0.into())]);
            output = Some(if workflow == 2 {
                let document = runtime.block_on(store.read(key, &pin, true)).unwrap();
                assert_eq!(document.content_hash, "original");
                assert_eq!(document.pin.object_id, JsString::from("remote"));
                (document.value, Json::Null)
            } else if workflow == 1 {
                let (document, metadata) = runtime
                    .block_on(store.read_with_metadata(key, &pin, true, false))
                    .unwrap();
                assert_eq!(document.content_hash, "original");
                assert_eq!(document.pin.object_id, JsString::from("remote"));
                (document.value, metadata)
            } else {
                let mut record = runtime
                    .block_on(reads.read_document(key, &pin, true, false, false))
                    .unwrap();
                (
                    record.remove("value").unwrap(),
                    record.remove("metadata").unwrap(),
                )
            });
        });
        let (value, metadata) = output.unwrap();
        let payload = if key == "task-board/history" {
            &value
        } else {
            f(&value, "opaque")
        };
        assert_eq!(payload.as_array().unwrap().as_ptr(), value_pointer);
        let retained_text = match key {
            "task-board/run" => Some(
                f(f(&value, "executionContext"), "latestResult")
                    .as_js_string()
                    .unwrap()
                    .0
                    .as_ptr(),
            ),
            "task-board/result" => Some(
                f(f(&value, "content"), "summary")
                    .as_js_string()
                    .unwrap()
                    .0
                    .as_ptr(),
            ),
            _ => None,
        };
        assert_eq!(retained_text, text_pointer);
        if workflow != 2 {
            assert_eq!(metadata.as_object().unwrap().as_ptr(), metadata_pointer);
        }
        assert!(source.0.lock().unwrap().is_none());
        eprintln!(
            "Owned record read: key={key}, workflow={workflow}, cached={cached}, additional={allocated} bytes beside owned source"
        );
        assert!(
            allocated < 48 * 1024,
            "workflow={workflow}, cached={cached}: read/cache admission exceeded its allocation budget ({allocated} bytes)"
        );
    }
    // Small admitted records exercise the other side of the boundary: the
    // cache keeps one original, while an owned caller copies selected fields.
    // Repeated reads must release those copies and the action must release its
    // cache. The metadata and revision references dominate the small value.
    for workflow in 0..4 {
        use ivy_task_board::engine_read::read_policy::TaskReadStore;
        let value = Json::Array(vec![Json::Null; 64]);
        let value_bytes = value.retained_bytes();
        let source = Arc::new(Source(Mutex::new(Some(Json::object([
            (
                "object",
                Json::object([
                    ("id", "remote".into()),
                    ("contractKey", "task-board/history".into()),
                    ("parentId", "root".into()),
                    ("effectivelyArchived", false.into()),
                    (
                        "opaque",
                        Json::String(JsString(vec![0xd800, 0x61].repeat(4096))),
                    ),
                ]),
            ),
            (
                "revision",
                Json::object([
                    ("revision", 7.0.into()),
                    ("contractVersion", "1.0.0".into()),
                    ("mediaType", "application/json".into()),
                    ("contentHash", "original".into()),
                    ("byteLength", 321.0.into()),
                    ("references", Json::Array(vec![Json::Null; 256])),
                ]),
            ),
            (
                "content",
                Json::object([("encoding", "json".into()), ("value", value)]),
            ),
        ])))));
        let budget = WorkBudget::default();
        let scopes = ReadScopes::with_budget(budget.clone());
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: source.clone(),
            contracts: Catalog,
            limits: HistoryLimits {
                count: 128,
                bytes: 16 * 1024 * 1024,
            },
            now: || "now".into(),
            new_id: || "unused".into(),
            cache: Some(scopes.cache()),
        };
        let local = SqliteReads {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            contracts: &Catalog,
            parse: |raw: &str| {
                ivy_contracts::json::parse(raw, ivy_contracts::limits::MANAGEMENT_FRAME_BYTES)
            },
            sqlite_failure: ivy_store::sqlite::error,
        };
        let comments = TaskComments::new(Arc::new(NoComments), runtime.handle().clone());
        let source_failure = |error| error;
        let reads: PersistentReadStore<'_, _, _, _, _, _, _, IvyError> = PersistentReadStore {
            local: &local,
            objects: source.as_ref(),
            policies: &Policies,
            comments: &comments,
            source_failure: &source_failure,
            cache: Some(scopes.cache()),
        };
        let pin = Json::object([("objectId", "remote".into()), ("revision", 7.0.into())]);
        let read = || {
            let value = match workflow {
                0 => {
                    let mut full =
                        ready(reads.read_document("task-board/history", &pin, true, false, false))
                            .unwrap();
                    let metadata = full
                        .required("metadata")
                        .unwrap()
                        .required("opaque")
                        .unwrap()
                        .as_js_string()
                        .unwrap();
                    assert_eq!(metadata.0.len(), 8192);
                    assert_eq!(metadata.0[0], 0xd800);
                    assert_eq!(
                        f(f(&full, "revision"), "references")
                            .as_array()
                            .unwrap()
                            .len(),
                        256
                    );
                    full.remove("value").unwrap()
                }
                1 => {
                    let (document, metadata) =
                        ready(store.read_with_metadata("task-board/history", &pin, true, false))
                            .unwrap();
                    assert_eq!(f(&metadata, "opaque").as_js_string().unwrap().0.len(), 8192);
                    document.value
                }
                2 => {
                    ready(store.read("task-board/history", &pin, true))
                        .unwrap()
                        .value
                }
                3 => ready(TaskReadStore::read(
                    &reads,
                    "task-board/history",
                    &pin,
                    false,
                ))
                .unwrap(),
                _ => unreachable!(),
            };
            assert_eq!(value.as_array().unwrap().len(), 64);
            assert!(value.as_array().unwrap().iter().all(Json::is_null));
        };
        let before = LIVE.load(Ordering::Relaxed);
        runtime.block_on(scopes.with_read_cache(async {
            let cold = allocations(read);
            let charged = budget.retained_usage();
            assert_eq!(charged.0, 0);
            assert!(charged.1 > value_bytes + 16 * 1024 && charged.1 < 64 * 1024);
            for batch in 0..3 {
                let started = std::time::Instant::now();
                let warm = allocations(|| { for _ in 0..32 { read(); } });
                assert_eq!(budget.retained_usage(), charged);
                assert_eq!(warm.3, 0, "warm reads retained caller-owned copies");
                if workflow >= 2 {
                    assert!(warm.0 < value_bytes as isize + 8 * 1024,
                        "value/document readers copied unused aggregate fields: {warm:?}");
                }
                eprintln!("Admitted record batch: workflow={workflow}, batch={batch}, reads=32, cold={cold:?}, warm={warm:?}, cache={charged:?}, elapsedNanos={}", started.elapsed().as_nanos());
            }
        }));
        assert!(
            source.0.lock().unwrap().is_none(),
            "the original entered the cache"
        );
        assert_eq!(budget.retained_usage(), (0, 0));
        assert!(
            LIVE.load(Ordering::Relaxed) < before - 16 * 1024,
            "the action retained its source metadata or cache after completion"
        );
    }
}

#[test]
fn owned_record_upgrades_retain_large_task_fields_and_conversation_buffers() {
    if isolated("owned_record_upgrades_retain_large_task_fields_and_conversation_buffers") {
        return;
    }
    use ivy_task_board::journal_store::persistent_reads::record_upgrades::{
        upgrade_stored_owned_with, upgrade_stored_with,
    };
    let input = Json::object([
        (
            "fields",
            Json::object([(
                "description",
                Json::String(JsString(vec![0xd800, 65, 0xdfff, 0].repeat(262144))),
            )]),
        ),
        (
            "comments",
            Json::Array(
                (0..512)
                    .map(|index| {
                        Json::object([
                            ("commentId", format!("comment-{index}").into()),
                            (
                                "body",
                                Json::String(JsString(vec![0xd800, 0, 65, 0xdfff].repeat(128))),
                            ),
                            ("handoff", Json::object([("wakeAt", Json::Null)])),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("opaque", Json::Number(f64::NAN)),
    ]);
    let description = f(f(&input, "fields"), "description")
        .as_js_string()
        .unwrap()
        .0
        .as_ptr();
    let comments = array(f(&input, "comments"));
    let conversation = comments.as_ptr();
    let bodies: Vec<_> = comments
        .iter()
        .map(|comment| f(comment, "body").as_js_string().unwrap().0.as_ptr())
        .collect();
    let copied = peak(|| {
        ready(upgrade_stored_with::<IvyError, _, _>(
            "task-board/task",
            &input,
            |_| async { panic!("current Task does not read historical evidence") },
        ))
        .unwrap();
    });
    let mut output = None;
    let moved = peak(|| {
        output = Some(
            ready(upgrade_stored_owned_with::<IvyError, _, _>(
                "task-board/task",
                input,
                |_| async { panic!("current Task does not read historical evidence") },
            ))
            .unwrap(),
        );
    });
    let output = output.unwrap();
    assert_eq!(
        f(f(&output, "fields"), "description")
            .as_js_string()
            .unwrap()
            .0
            .as_ptr(),
        description
    );
    assert_eq!(array(f(&output, "comments")).as_ptr(), conversation);
    for (comment, body) in array(f(&output, "comments")).iter().zip(bodies) {
        assert_eq!(f(comment, "body").as_js_string().unwrap().0.as_ptr(), body);
    }
    assert!(f(&output, "opaque").as_f64().unwrap().is_nan());
    assert!(copied > 2 * 1024 * 1024, "borrowed copy peak: {copied}");
    assert!(
        moved < 128 * 1024,
        "owned upgrade copied retained payloads: {moved}"
    );
    eprintln!(
        "Owned Task upgrade: borrowed={copied}, moved={moved} bytes beside owned input (512 comments, 2MiB description)"
    );
}

#[test]
fn cached_comment_nodes_keep_original_buffers_hashes_and_bounded_read_work() {
    if isolated("cached_comment_nodes_keep_original_buffers_hashes_and_bounded_read_work") {
        return;
    }
    use ivy_sdk::task_comments::{CancellationToken, CommentReadFuture, CommentSource};
    use ivy_task_board::journal_store::persistent_reads::task_comments::TaskComments;
    use std::sync::{Arc, Mutex};
    struct Source {
        saved: Mutex<Option<Json>>,
        calls: AtomicUsize,
    }
    impl CommentSource for Source {
        type Error = IvyError;
        fn read<'a>(
            &'a self,
            pin: Json,
            _: Option<CancellationToken>,
        ) -> CommentReadFuture<'a, IvyError> {
            Box::pin(async move {
                assert_eq!(f(&pin, "objectId"), &Json::from("node"));
                self.calls.fetch_add(1, Ordering::Relaxed);
                Ok(self
                    .saved
                    .lock()
                    .unwrap()
                    .take()
                    .expect("one immutable source read"))
            })
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let body = Json::String(JsString(vec![0xd800, 0, 65, 0xdfff].repeat(8192)));
    let pointer = body.as_js_string().unwrap().0.as_ptr();
    let node = Json::object([
        ("schemaVersion", 1.0.into()),
        ("taskId", "task".into()),
        ("kind", "comment".into()),
        (
            "entry",
            Json::object([
                (
                    "comment",
                    Json::object([
                        ("commentId", "comment".into()),
                        ("sequence", 1.0.into()),
                        ("body", body),
                        (
                            "attachments",
                            Json::Array(
                                (0..32)
                                    .map(|index| {
                                        Json::object([
                                            ("attachmentId", format!("attachment-{index}").into()),
                                            (
                                                "object",
                                                Json::object([
                                                    ("objectId", format!("file-{index}").into()),
                                                    ("revision", 1.0.into()),
                                                ]),
                                            ),
                                            ("filename", "retained.txt".into()),
                                            ("mediaType", "text/plain".into()),
                                            ("byteLength", 1.0.into()),
                                            (
                                                "contentHash",
                                                ivy_contracts::hash::digest(b"x").into(),
                                            ),
                                            ("createdAt", "2026-01-02T03:04:05.006Z".into()),
                                        ])
                                    })
                                    .collect(),
                            ),
                        ),
                    ]),
                ),
                ("receipt", Json::Null),
            ]),
        ),
    ]);
    let expected_hash = ivy_contracts::hash::hash_json(&node).unwrap();
    let source = Arc::new(Source {
        saved: Mutex::new(Some(Json::object([
            (
                "object",
                Json::object([
                    ("id", "node".into()),
                    ("contractKey", "task-board/comment-node".into()),
                    ("parentId", "task".into()),
                    ("ownerObjectId", "task".into()),
                ]),
            ),
            (
                "revision",
                Json::object([
                    ("revision", 1.0.into()),
                    ("contractVersion", "2.0.0".into()),
                ]),
            ),
            (
                "content",
                Json::object([("encoding", "json".into()), ("value", node)]),
            ),
        ]))),
        calls: AtomicUsize::new(0),
    });
    let comments = TaskComments::new(source.clone(), runtime.handle().clone());
    let pin = Json::object([("objectId", "node".into()), ("revision", 1.0.into())]);
    let owner = JsString::from("task");
    let original = runtime.block_on(comments.node(&pin, &owner, None)).unwrap();
    assert_eq!(
        f(f(f(&original, "entry"), "comment"), "body")
            .as_js_string()
            .unwrap()
            .0
            .as_ptr(),
        pointer
    );
    assert_eq!(comments.pin_for_hash(&expected_hash), Some(pin.clone()));
    assert!(
        runtime
            .block_on(comments.node(&pin, &"other".into(), None))
            .is_err()
    );
    // A caller that edits its own Arc must leave both the cached original and
    // its write-pin identity untouched.
    {
        let mut edited = original.clone();
        Arc::make_mut(&mut edited).insert("taskId", "caller-copy".into());
        assert_ne!(
            ivy_contracts::hash::hash_json(&edited).unwrap(),
            expected_hash
        );
    }
    for batch in 0..3 {
        let started = std::time::Instant::now();
        let warm = allocations(|| {
            for _ in 0..64 {
                let node = runtime.block_on(comments.node(&pin, &owner, None)).unwrap();
                assert!(Arc::ptr_eq(&original, &node));
            }
        });
        let elapsed = started.elapsed().as_nanos();
        assert_eq!(warm.3, 0, "cache hits retained another node or pin copy");
        assert!(
            warm.0 < 32 * 1024,
            "cache hits copied the retained comment node: {warm:?}"
        );
        assert!(
            warm.1 < 1024 * 1024,
            "cache hits repeatedly allocated escaped text: {warm:?}"
        );
        assert_eq!(source.calls.load(Ordering::Relaxed), 1);
        assert_eq!(comments.pin_for_hash(&expected_hash), Some(pin.clone()));
        eprintln!(
            "Cached comment node: batch={batch}, reads=64, warm={warm:?}, elapsedNanos={elapsed}"
        );
    }
    runtime.block_on(comments.settled());
}

#[test]
fn owned_task_hydration_preserves_headers_and_bounds_conversation_copies() {
    if isolated("owned_task_hydration_preserves_headers_and_bounds_conversation_copies") {
        return;
    }
    use ivy_sdk::{
        rpc::{RequestFuture, RequestOptions, RpcClient},
        task_comments::{CancellationToken, CommentReadFuture, CommentSource},
    };
    use ivy_task_board::{
        engine_read::read_policy::QuestionPolicies,
        journal_store::{
            Contracts, JournalStore, ReadCache,
            persistent_reads::{
                local_reads::SqliteReads,
                store::{ObjectReads, PersistentReadStore},
                task_comments::TaskComments,
            },
        },
        local_journal::{LocalJournal, initialize},
        local_retention::HistoryLimits,
    };
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };
    struct Catalog;
    impl Contracts for Catalog {
        fn version(&self, _: &str) -> IvyResult<String> {
            Ok("3.5.0".into())
        }
        fn local(&self, _: &str) -> IvyResult<bool> {
            Ok(false)
        }
        fn readable_version(&self, _: &str, version: &JsString) -> IvyResult<bool> {
            Ok(version == &JsString::from("3.5.0"))
        }
        fn validate_record(&self, _: &str, _: &JsString, _: &Json) -> IvyResult<()> {
            Ok(())
        }
        fn validate_definition(&self, _: &str, _: &Json) -> IvyResult<()> {
            unreachable!()
        }
    }
    struct Source(Mutex<VecDeque<(Json, Json)>>);
    impl RpcClient for Source {
        fn request<'a>(
            &'a self,
            method: &'a str,
            params: Json,
            _: RequestOptions,
        ) -> RequestFuture<'a> {
            Box::pin(async move {
                assert_eq!(method, "objects.read");
                let (id, value) = self
                    .0
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("one original read per object");
                assert_eq!(params.get("objectId"), Some(&id));
                Ok(value)
            })
        }
    }
    impl ObjectReads<IvyError> for Source {
        async fn read(&self, params: Option<Json>) -> IvyResult<Json> {
            self.request("objects.read", params.unwrap(), RequestOptions::default())
                .await
        }
    }
    impl CommentSource for Source {
        type Error = IvyError;
        fn read<'a>(
            &'a self,
            pin: Json,
            _: Option<CancellationToken>,
        ) -> CommentReadFuture<'a, IvyError> {
            self.request("objects.read", pin, RequestOptions::default())
        }
    }
    struct Policies;
    impl QuestionPolicies<IvyError> for Policies {
        fn open_questions(&self, task: &Json) -> IvyResult<Vec<Json>> {
            ivy_sdk::open_task_questions(task, None)
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(tokio::task::yield_now());
    let db = ivy_store::rusqlite::Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    for format in ["inline", "prefix", "tree"] {
        for workflow in [false, true] {
            for cached in [false, true] {
                let comment = Json::object([
                    ("commentId", "c1".into()),
                    ("sequence", 1.0.into()),
                    ("authorKind", "user".into()),
                    ("purpose", "update".into()),
                    (
                        "body",
                        Json::String(JsString(vec![0xd800, 0, 65, 0xdfff].repeat(65536))),
                    ),
                    ("attachments", Json::Array(vec![])),
                    ("requests", Json::Array(vec![])),
                    ("responses", Json::Array(vec![])),
                ]);
                let body = f(&comment, "body").as_js_string().unwrap().0.as_ptr();
                let body_hash = ivy_contracts::hash::hash_json(f(&comment, "body")).unwrap();
                let receipt =
                    Json::object([("commentId", "c1".into()), ("state", "queued".into())]);
                let mut task = Json::object([
                    (
                        "fields",
                        Json::object([(
                            "description",
                            Json::String(JsString(vec![0xd800, 65, 0xdfff, 0].repeat(262144))),
                        )]),
                    ),
                    ("comments", Json::Array(vec![])),
                    ("commentDeliveries", Json::Array(vec![receipt.clone()])),
                    (
                        "opaque",
                        Json::Object(vec![
                            ("same".into(), (-0.0).into()),
                            ("same".into(), "retained".into()),
                        ]),
                    ),
                ]);
                let description = f(f(&task, "fields"), "description")
                    .as_js_string()
                    .unwrap()
                    .0
                    .as_ptr();
                let opaque = ivy_contracts::hash::hash_json(f(&task, "opaque")).unwrap();
                let mut reads = VecDeque::new();
                if format == "inline" {
                    task.insert("comments", Json::Array(vec![comment]));
                } else if format == "prefix" {
                    task.insert(
                        "commentStore",
                        Json::object([
                            ("objectId", "log".into()),
                            ("length", 1.0.into()),
                            ("nativeInputs", Json::Array(vec![])),
                        ]),
                    );
                    reads.push_back((
                        "log".into(),
                        Json::object([
                            (
                                "object",
                                Json::object([
                                    ("contractKey", "task-board/comments".into()),
                                    ("ownerObjectId", Json::Null),
                                    ("parentId", "task".into()),
                                ]),
                            ),
                            (
                                "content",
                                Json::object([
                                    ("encoding", "json".into()),
                                    (
                                        "value",
                                        Json::object([
                                            ("taskId", "task".into()),
                                            ("comments", Json::Array(vec![comment])),
                                        ]),
                                    ),
                                ]),
                            ),
                        ]),
                    ));
                } else {
                    let pin = Json::object([("objectId", "node".into()), ("revision", 1.0.into())]);
                    task.insert(
                        "commentStore",
                        Json::object([
                            ("kind", "tree".into()),
                            (
                                "root",
                                Json::object([("object", pin), ("length", 1.0.into())]),
                            ),
                            ("length", 1.0.into()),
                        ]),
                    );
                    reads.push_back((
                        "node".into(),
                        Json::object([
                            (
                                "object",
                                Json::object([
                                    ("id", "node".into()),
                                    ("contractKey", "task-board/comment-node".into()),
                                    ("parentId", "task".into()),
                                    ("ownerObjectId", "task".into()),
                                ]),
                            ),
                            (
                                "revision",
                                Json::object([
                                    ("revision", 1.0.into()),
                                    ("contractVersion", "2.0.0".into()),
                                ]),
                            ),
                            (
                                "content",
                                Json::object([
                                    ("encoding", "json".into()),
                                    (
                                        "value",
                                        Json::object([
                                            ("schemaVersion", 1.0.into()),
                                            ("taskId", "task".into()),
                                            ("kind", "comment".into()),
                                            (
                                                "entry",
                                                Json::object([
                                                    ("comment", comment),
                                                    ("receipt", receipt.clone()),
                                                ]),
                                            ),
                                        ]),
                                    ),
                                ]),
                            ),
                        ]),
                    ));
                }
                reads.push_front((
                    "task".into(),
                    Json::object([
                        (
                            "object",
                            Json::object([
                                ("id", "task".into()),
                                ("contractKey", "task-board/task".into()),
                                ("parentId", "root".into()),
                                ("effectivelyArchived", false.into()),
                            ]),
                        ),
                        (
                            "revision",
                            Json::object([
                                ("revision", 7.0.into()),
                                ("contractVersion", "3.5.0".into()),
                                ("mediaType", "application/json".into()),
                                ("contentHash", "original".into()),
                                ("byteLength", 3000000.0.into()),
                            ]),
                        ),
                        (
                            "content",
                            Json::object([("encoding", "json".into()), ("value", task)]),
                        ),
                    ]),
                ));
                let source = Arc::new(Source(Mutex::new(reads)));
                let cache = ReadCache::default();
                let store = JournalStore {
                    journal: LocalJournal {
                        connection: &db,
                        root_object_id: Some("root".into()),
                    },
                    client: source.clone(),
                    contracts: Catalog,
                    limits: HistoryLimits {
                        count: 128,
                        bytes: 16 * 1024 * 1024,
                    },
                    now: || "now".into(),
                    new_id: || "unused".into(),
                    cache: cached.then_some(&cache),
                };
                let local = SqliteReads {
                    journal: LocalJournal {
                        connection: &db,
                        root_object_id: Some("root".into()),
                    },
                    contracts: &Catalog,
                    parse: |raw: &str| {
                        ivy_contracts::json::parse(
                            raw,
                            ivy_contracts::limits::MANAGEMENT_FRAME_BYTES,
                        )
                    },
                    sqlite_failure: ivy_store::sqlite::error,
                };
                let comments = TaskComments::new(source.clone(), runtime.handle().clone());
                let source_failure = |error| error;
                let reader: PersistentReadStore<'_, _, _, _, _, _, _, IvyError> =
                    PersistentReadStore {
                        local: &local,
                        objects: source.as_ref(),
                        policies: &Policies,
                        comments: &comments,
                        source_failure: &source_failure,
                        cache: cached.then_some(&cache),
                    };
                let mut output = None;
                let allocated = peak(|| {
                    let pin = Json::object([("objectId", "task".into()), ("revision", 7.0.into())]);
                    output = Some(if workflow {
                        runtime
                            .block_on(store.read("task-board/task", &pin, true))
                            .unwrap()
                            .value
                    } else {
                        runtime
                            .block_on(reader.read_document(
                                "task-board/task",
                                &pin,
                                true,
                                false,
                                false,
                            ))
                            .unwrap()
                            .remove("value")
                            .unwrap()
                    });
                });
                let output = output.unwrap();
                assert_eq!(
                    f(f(&output, "fields"), "description")
                        .as_js_string()
                        .unwrap()
                        .0
                        .as_ptr(),
                    description
                );
                assert_eq!(
                    ivy_contracts::hash::hash_json(f(&output, "opaque")).unwrap(),
                    opaque
                );
                assert!(output.get("commentStore").is_none());
                let body_value = f(&array(f(&output, "comments"))[0], "body");
                assert_eq!(
                    ivy_contracts::hash::hash_json(body_value).unwrap(),
                    body_hash
                );
                if format != "tree" {
                    assert_eq!(body_value.as_js_string().unwrap().0.as_ptr(), body);
                }
                assert_eq!(array(f(&output, "commentDeliveries")), &[receipt]);
                assert!(source.0.lock().unwrap().is_empty());
                // The immutable cache and the owned range still need separate entries.
                // Hydration must retain the 2MiB header and add no further body copy.
                let budget = if format == "tree" {
                    768 * 1024
                } else {
                    64 * 1024
                };
                assert!(
                    allocated < budget,
                    "{format}, workflow={workflow}, cached={cached}: {allocated} bytes"
                );
                eprintln!(
                    "Owned Task hydration: format={format}, workflow={workflow}, cached={cached}, additional={allocated} bytes beside owned source"
                );
            }
        }
    }
}

struct OriginalOperation {
    documents: RefCell<Vec<Document>>,
    reads: Cell<usize>,
    writes: Cell<usize>,
    completions: Cell<usize>,
    original_fields: Option<(usize, usize)>,
    write_check: Option<Box<dyn Fn(&Json)>>,
}
impl OriginalOperation {
    fn new(value: Json) -> Self {
        Self::records(vec![value])
    }
    fn records(values: Vec<Json>) -> Self {
        Self {
            documents: RefCell::new(
                values
                    .into_iter()
                    .map(|value| Document {
                        pin: ivy_task_board::operations::ObjectPin::from_json(Json::object([
                            ("objectId", "operation".into()),
                            ("revision", Json::Number(7.0)),
                        ]))
                        .unwrap(),
                        content_hash: ivy_contracts::hash::hash_json(&value).unwrap(),
                        value,
                        parent_id: None,
                    })
                    .collect(),
            ),
            reads: Cell::new(0),
            writes: Cell::new(0),
            completions: Cell::new(0),
            original_fields: None,
            write_check: None,
        }
    }
    fn take(&self) -> Document {
        self.reads.set(self.reads.get() + 1);
        self.documents.borrow_mut().remove(0)
    }
}
impl Store for OriginalOperation {
    fn root_object_id(&self) -> Option<JsString> {
        unreachable!()
    }
    fn contract_version(&self, _: &str) -> IvyResult<String> {
        unreachable!()
    }
    fn retired_operation(&self, _: &JsString, _: &JsString) -> IvyResult<bool> {
        unreachable!()
    }
    fn local_key(&self, _: &JsString) -> IvyResult<Option<String>> {
        unreachable!()
    }
    fn complete_local_workflow(&self, id: &JsString, at: &JsString) -> IvyResult<()> {
        assert!(self.original_fields.is_some(), "unexpected completion");
        assert_eq!(id, &JsString::from("operation"));
        assert_eq!(at, &JsString::from("now"));
        self.completions.set(self.completions.get() + 1);
        Ok(())
    }
    async fn named(&self, key: &str, _: &str, _: Option<&JsString>) -> IvyResult<Option<Document>> {
        assert_eq!(key, "task-board/operation");
        Ok(Some(self.take()))
    }
    async fn read(&self, key: &str, address: &Json, primary: bool) -> IvyResult<Document> {
        assert_eq!(key, "task-board/operation");
        assert!(primary);
        let document = self.take();
        if address.as_object().is_some() {
            assert_eq!(address, &document.pin.to_json());
        }
        Ok(document)
    }
    async fn write(&self, key: &str, value: &Json, _: &str, destination: &Json) -> IvyResult<Json> {
        assert_eq!(key, "task-board/operation");
        if let Some(check) = &self.write_check {
            check(value);
        } else {
            let (request, opaque) = self.original_fields.expect("receipt reads must not write");
            for (name, pointer) in [("request", request), ("opaque", opaque)] {
                assert_eq!(
                    value.required(name)?.as_js_string().unwrap().0.as_ptr() as usize,
                    pointer,
                    "owned transition copied retained {name}"
                );
            }
            assert_eq!(
                value
                    .as_object()
                    .unwrap()
                    .iter()
                    .filter(|(key, _)| key == &JsString::from("opaque"))
                    .count(),
                1
            );
            assert_eq!(value.get("phase"), Some(&"succeeded".into()));
        }
        assert_eq!(value.get("updatedAt"), Some(&"now".into()));
        assert_eq!(
            destination,
            &Json::object([
                ("objectId", "operation".into()),
                ("expectedRevision", Json::Number(7.0))
            ])
        );
        self.writes.set(self.writes.get() + 1);
        Ok(Json::object([
            ("objectId", "operation".into()),
            ("revision", Json::Number(8.0)),
        ]))
    }
    async fn prepared_mutation_id(&self, _: &str) -> IvyResult<JsString> {
        unreachable!()
    }
    async fn write_prepared(
        &self,
        _: &str,
        _: &Json,
        _: &JsString,
        _: &Json,
        _: Option<&JsString>,
        _: Option<&Json>,
    ) -> IvyResult<Json> {
        unreachable!("retained receipt must not publish again")
    }
}
fn ready<T>(future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    {
        std::task::Poll::Ready(result) => result,
        std::task::Poll::Pending => panic!("the in-memory source never suspends"),
    }
}
fn large_original() -> Json {
    Json::String(JsString(vec![0xd800, 0, 65, 0xdfff].repeat(131072)))
}
#[test]
fn operation_find_retains_native_document_buffers_and_raw_values() {
    let value = Json::Object(vec![
        ("callerPrincipalId".into(), "caller".into()),
        ("operationId".into(), "original".into()),
        ("opaque".into(), "first".into()),
        ("opaque".into(), large_original()),
    ]);
    let parent = JsString(vec![0xd800, 0, 65, 0xdfff].repeat(32));
    let hash = ivy_contracts::hash::hash_json(&value).unwrap();
    let expected = value.clone();
    let pointers = (
        value.as_object().unwrap().as_ptr(),
        parent.0.as_ptr(),
        hash.as_ptr(),
    );
    let store = OriginalOperation::new(value);
    {
        let mut documents = store.documents.borrow_mut();
        documents[0].parent_id = Some(parent);
        documents[0].content_hash = hash;
    }
    let operations = Operations {
        store,
        now: || unreachable!(),
    };
    let found = ready(operations.find(&"caller".into(), &"original".into()))
        .unwrap()
        .unwrap();
    assert_eq!(found.value, expected);
    assert_eq!(found.value.as_object().unwrap().as_ptr(), pointers.0);
    assert_eq!(found.parent_id.as_ref().unwrap().0.as_ptr(), pointers.1);
    assert_eq!(found.content_hash.as_ptr(), pointers.2);
    assert_eq!(operations.store.reads.get(), 1);
    assert_eq!(operations.store.writes.get(), 0);
}
#[test]
fn operation_transition_moves_original_fields_with_last_value_updates() {
    if isolated("operation_transition_moves_original_fields_with_last_value_updates") {
        return;
    }
    let request = large_original();
    let opaque = large_original();
    let pointers = (
        request.as_js_string().unwrap().0.as_ptr() as usize,
        opaque.as_js_string().unwrap().0.as_ptr() as usize,
    );
    let value = Json::Object(vec![
        ("operationId".into(), "original-operation".into()),
        ("opaque".into(), "discarded first value".into()),
        ("request".into(), request),
        ("opaque".into(), opaque),
        ("phase".into(), "publishing".into()),
        ("writes".into(), Json::Array(vec![])),
        ("outcome".into(), Json::Null),
        ("error".into(), Json::Null),
        ("updatedAt".into(), "before".into()),
    ]);
    let outcome = Json::object([
        ("operationId", "original-operation".into()),
        ("task", "published".into()),
    ]);
    let mut authoritative = Json::object(value.as_object().unwrap().iter().cloned());
    for (name, value) in [
        ("phase", "succeeded".into()),
        ("outcome", outcome.clone()),
        ("updatedAt", "now".into()),
    ] {
        authoritative.insert(name, value);
    }
    let original_hash_peak = peak(|| {
        drop(ivy_contracts::hash::digest(
            canonical(
                &authoritative,
                ivy_contracts::limits::MANAGEMENT_FRAME_BYTES,
            )
            .unwrap()
            .as_bytes(),
        ));
    });
    let mut store = OriginalOperation::records(vec![value, authoritative]);
    store.original_fields = Some(pointers);
    store.documents.borrow_mut()[1].pin.revision = 8.0;
    let clocks = Cell::new(0);
    let operations = Operations {
        store,
        now: || {
            clocks.set(clocks.get() + 1);
            "now".to_owned()
        },
    };
    let additional = peak(|| {
        assert_eq!(
            ready(operations.finish(&"operation".into(), &outcome)).unwrap(),
            outcome
        );
    });
    assert!(original_hash_peak > 2 * 1024 * 1024);
    assert!(
        additional < 64 * 1024,
        "operation transition retained a canonical text buffer: {additional}"
    );
    assert_eq!(operations.store.reads.get(), 2);
    assert_eq!(operations.store.writes.get(), 1);
    assert_eq!(operations.store.completions.get(), 1);
    assert_eq!(clocks.get(), 1);
    eprintln!(
        "Owned operation transition: original-hash={original_hash_peak}, streaming={additional} bytes beside owned original payloads"
    );
}
#[test]
fn completed_operation_compares_large_outcomes_without_canonical_text_buffers() {
    if isolated("completed_operation_compares_large_outcomes_without_canonical_text_buffers") {
        return;
    }
    for different in [false, true] {
        let saved = Json::Object(vec![
            ("operationId".into(), "original-operation".into()),
            ("body".into(), large_original()),
            ("zero".into(), Json::Number(-0.0)),
            (
                "detail".into(),
                Json::Object(vec![
                    ("x".into(), true.into()),
                    ("y".into(), Json::Null),
                    ("x".into(), false.into()),
                ]),
            ),
        ]);
        let pointer = saved
            .get("body")
            .unwrap()
            .as_js_string()
            .unwrap()
            .0
            .as_ptr();
        let expected = saved.clone();
        let mut proposed = Json::Object(vec![
            (
                "detail".into(),
                Json::Object(vec![
                    ("y".into(), Json::Null),
                    ("x".into(), true.into()),
                    ("x".into(), false.into()),
                ]),
            ),
            ("zero".into(), Json::Number(0.0)),
            ("body".into(), saved.get("body").unwrap().clone()),
            ("operationId".into(), "original-operation".into()),
        ]);
        if different {
            let Json::String(body) = proposed.get_mut("body").unwrap() else {
                unreachable!()
            };
            *body.0.last_mut().unwrap() = 66;
        }
        let original = peak(|| {
            assert_eq!(
                canonical(&saved, ivy_contracts::limits::MANAGEMENT_FRAME_BYTES).unwrap()
                    == canonical(&proposed, ivy_contracts::limits::MANAGEMENT_FRAME_BYTES).unwrap(),
                !different
            );
        });
        let mut store = OriginalOperation::new(Json::object([
            ("operationId", "original-operation".into()),
            ("phase", "succeeded".into()),
            ("updatedAt", "now".into()),
            ("outcome", saved),
        ]));
        store.original_fields = Some((0, 0));
        let clocks = Cell::new(0);
        let operations = Operations {
            store,
            now: || {
                clocks.set(clocks.get() + 1);
                "unexpected".into()
            },
        };
        let id = JsString::from("operation");
        let mut result = None;
        let additional = peak(|| {
            result = Some(ready(operations.finish(&id, &proposed)));
        });
        let result = result.unwrap();
        if different {
            assert_eq!(result.unwrap_err().code, "mutation_conflict");
        } else {
            let result = result.unwrap();
            assert_eq!(
                result
                    .get("body")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .0
                    .as_ptr(),
                pointer
            );
            assert_eq!(
                canonical(&result, ivy_contracts::limits::MANAGEMENT_FRAME_BYTES).unwrap(),
                canonical(&expected, ivy_contracts::limits::MANAGEMENT_FRAME_BYTES).unwrap()
            );
        }
        assert_eq!(operations.store.reads.get(), 1);
        assert_eq!(operations.store.writes.get(), 0);
        assert_eq!(operations.store.completions.get(), usize::from(!different));
        assert_eq!(clocks.get(), 0);
        assert!(original > 4 * 1024 * 1024);
        assert!(
            additional < 64 * 1024,
            "Operation equality retained canonical text: {additional}"
        );
        eprintln!(
            "Completed operation equality: different={different}, original={original}, borrowed={additional} bytes beside owned original payloads"
        );
    }
}
#[test]
fn publication_replay_skips_unused_operation_and_absent_callback_payloads() {
    if isolated("publication_replay_skips_unused_operation_and_absent_callback_payloads") {
        return;
    }
    let destination = Json::object([
        ("objectId", "task".into()),
        ("expectedRevision", Json::Number(2.0)),
    ]);
    let receipt = Json::object([("objectId", "task".into()), ("revision", Json::Number(3.0))]);
    let saved = Json::object([
        ("request", large_original()),
        ("phase", "succeeded".into()),
        (
            "writes",
            Json::Array(vec![Json::object([
                ("contractKey", "task-board/task".into()),
                ("destination", destination.clone()),
                ("outcome", receipt.clone()),
            ])]),
        ),
    ]);
    let operations = Operations {
        store: OriginalOperation::records(vec![saved.clone(), saved]),
        now: || panic!("retained publication does not advance the clock"),
    };
    let operation = Document {
        pin: ivy_task_board::operations::ObjectPin::from_json(Json::object([
            ("objectId", "operation".into()),
            ("revision", Json::Number(7.0)),
        ]))
        .unwrap(),
        value: large_original(),
        parent_id: None,
        content_hash: String::new(),
    };
    let proposed = Json::Object(vec![
        ("opaque".into(), large_original()),
        ("opaque".into(), "raw duplicate".into()),
    ]);
    let mut publication = Publication {
        operations: &operations,
        operation: &operation,
        task_written: None,
        index: 0.0,
    };
    let additional = peak(|| {
        assert_eq!(
            ready(publication.write("task-board/task", &proposed, &destination)).unwrap(),
            receipt
        )
    });
    assert!(
        additional < 64 * 1024,
        "publication copied an unused operation/callback payload: {additional}"
    );
    assert_eq!(publication.index, 1.0);
    assert_eq!(operations.store.reads.get(), 2);
    assert_eq!(operations.store.writes.get(), 0);
    eprintln!(
        "Borrowed publication replay: additional={additional} bytes beside owned original payloads"
    );
}
#[test]
fn native_call_updates_move_original_arrays_and_selected_results() {
    if isolated("native_call_updates_move_original_arrays_and_selected_results") {
        return;
    }
    for observe in [false, true] {
        let mut calls = Vec::with_capacity(3);
        calls.push(Json::object([
            ("key", "retained".into()),
            ("operationId", "retained-native".into()),
            ("observation", Json::Null),
            ("opaque", large_original()),
        ]));
        let proposed = Json::object([
            ("key", "selected".into()),
            ("operationId", "native-operation".into()),
            ("observation", Json::Null),
        ]);
        let observation = Json::object([
            ("operationId", "native-operation".into()),
            ("phase", "succeeded".into()),
        ]);
        if observe {
            let mut selected = proposed.clone();
            selected.insert("opaque", large_original());
            calls.push(selected);
        }
        let array_pointer = calls.as_ptr() as usize;
        let pointers: Vec<_> = calls
            .iter()
            .map(|call| {
                call.required("opaque")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .0
                    .as_ptr() as usize
            })
            .collect();
        let mut updated = calls.clone();
        if observe {
            updated[1].insert("observation", observation.clone());
        } else {
            updated.push(proposed.clone());
        }
        let result_pointer = observe.then(|| {
            updated[1]
                .required("opaque")
                .unwrap()
                .as_js_string()
                .unwrap()
                .0
                .as_ptr()
        });
        let expected = updated[1].clone();
        let value = Json::Object(vec![
            ("phase".into(), "preparing".into()),
            ("nativeCalls".into(), Json::Array(calls)),
            ("between".into(), "original position".into()),
            ("nativeCalls".into(), Json::Bool(false)),
            ("updatedAt".into(), "before".into()),
        ]);
        let mut authoritative = Json::object(value.as_object().unwrap().iter().cloned());
        authoritative.insert("nativeCalls", Json::Array(updated));
        authoritative.insert("updatedAt", "now".into());
        let mut store = OriginalOperation::records(vec![value, authoritative]);
        store.documents.borrow_mut()[1].pin.revision = 8.0;
        store.write_check = Some(Box::new(move |value| {
            let fields = value.as_object().unwrap();
            let names = ["phase", "nativeCalls", "between", "updatedAt"];
            assert_eq!(fields.len(), names.len());
            assert!(
                fields
                    .iter()
                    .zip(names)
                    .all(|((key, _), name)| key.eq_str(name))
            );
            assert_eq!(value.get("phase"), Some(&"preparing".into()));
            let calls = value.required("nativeCalls").unwrap().as_array().unwrap();
            assert_eq!(
                calls.as_ptr() as usize,
                array_pointer,
                "copied original calls array"
            );
            for (index, pointer) in pointers.iter().enumerate() {
                assert_eq!(
                    calls[index]
                        .required("opaque")
                        .unwrap()
                        .as_js_string()
                        .unwrap()
                        .0
                        .as_ptr() as usize,
                    *pointer,
                    "copied retained native call"
                );
            }
            assert_eq!(calls.len(), 2);
        }));
        let clocks = Cell::new(0);
        let operations = Operations {
            store,
            now: || {
                clocks.set(clocks.get() + 1);
                "now".to_owned()
            },
        };
        let mut result = None;
        let additional = peak(|| {
            result = Some(if observe {
                ready(operations.observe_native(
                    &"operation".into(),
                    &"selected".into(),
                    &observation,
                ))
                .unwrap()
            } else {
                ready(operations.prepare_native(&"operation".into(), &proposed)).unwrap()
            });
        });
        let result = result.unwrap();
        assert_eq!(result, expected);
        if let Some(pointer) = result_pointer {
            assert_eq!(
                result
                    .required("opaque")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .0
                    .as_ptr(),
                pointer,
                "copied selected authoritative result"
            );
        }
        assert!(
            additional < 64 * 1024,
            "native call update copied original arrays: {additional}"
        );
        assert_eq!(operations.store.reads.get(), 2);
        assert_eq!(operations.store.writes.get(), 1);
        assert_eq!(operations.store.completions.get(), 0);
        assert_eq!(clocks.get(), 1);
        eprintln!(
            "Owned native call update: observe={observe}, additional={additional} bytes beside owned original payloads"
        );
    }
}
#[test]
fn operation_reads_borrow_large_original_fields_and_indexed_receipts() {
    if isolated("operation_reads_borrow_large_original_fields_and_indexed_receipts") {
        return;
    }
    let caller = JsString::from("caller");
    let id = JsString::from("original-operation");
    let body = large_original();
    let original_pointer = body.as_js_string().unwrap().0.as_ptr();
    let value = Json::object([
        ("callerPrincipalId", Json::String(caller.clone())),
        ("operationId", Json::String(id.clone())),
        (
            "request",
            Json::Object(vec![
                ("opaque".into(), body),
                ("opaque".into(), "later duplicate".into()),
            ]),
        ),
    ]);
    let operations = Operations {
        store: OriginalOperation::new(value),
        now: || panic!("reads do not advance the clock"),
    };
    let mut found = None;
    let find_peak = peak(|| {
        found = ready(operations.find(&caller, &id)).unwrap();
    });
    let found = found.unwrap();
    assert_eq!(operations.store.reads.get(), 1);
    assert_eq!(
        found
            .value
            .required("request")
            .unwrap()
            .required("opaque")
            .unwrap()
            .as_js_string()
            .unwrap()
            .0
            .as_ptr(),
        original_pointer
    );
    assert_eq!(
        found
            .value
            .required("request")
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        2
    );
    assert!(
        find_peak < 64 * 1024,
        "find copied original request: {find_peak}"
    );
    drop(found);
    let receipt = Json::object([
        ("objectId", "published".into()),
        ("revision", Json::Number(9.0)),
        ("opaque", large_original()),
    ]);
    let writes = Json::Array(vec![
        Json::object([("outcome", Json::Null), ("opaque", large_original())]),
        Json::object([("outcome", receipt.clone()), ("opaque", large_original())]),
    ]);
    let receipt_pointer = writes.as_array().unwrap()[1]
        .required("outcome")
        .unwrap()
        .required("opaque")
        .unwrap()
        .as_js_string()
        .unwrap()
        .0
        .as_ptr();
    let operations = Operations {
        store: OriginalOperation::new(Json::Object(vec![
            ("writes".into(), writes),
            ("writes".into(), Json::Bool(false)),
            ("phase".into(), "succeeded".into()),
        ])),
        now: || panic!("receipt reads do not advance the clock"),
    };
    let mut result = None;
    let apply_peak = peak(|| {
        result = Some(ready(operations.apply(&id, 1)).unwrap());
    });
    let result = result.unwrap();
    assert_eq!(result, receipt);
    assert_eq!(
        result
            .required("opaque")
            .unwrap()
            .as_js_string()
            .unwrap()
            .0
            .as_ptr(),
        receipt_pointer
    );
    assert_eq!(operations.store.reads.get(), 1);
    assert!(
        apply_peak < 64 * 1024,
        "apply copied original writes or entry: {apply_peak}"
    );
    for (writes, code, message) in [
        (
            Json::Array(vec![]),
            "task_board_stage_conflict",
            "No write has been prepared at this stage.",
        ),
        (Json::Bool(false), "invalid_arguments", "Expected an array."),
    ] {
        let operations = Operations {
            store: OriginalOperation::new(Json::object([("writes", writes)])),
            now: || panic!("invalid reads do not advance the clock"),
        };
        let error = ready(operations.apply(&id, 0)).unwrap_err();
        assert_eq!(error.code.as_str(), code);
        assert_eq!(error.message_js(), JsString::from(message));
        assert_eq!(operations.store.reads.get(), 1);
    }
    eprintln!(
        "Borrowed operation access: find={find_peak}, retained-apply={apply_peak} bytes beside owned original payloads"
    );
}
fn f<'a>(v: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(key).unwrap_or(&NULL)
}
fn array(v: &Json) -> &[Json] {
    v.as_array().unwrap_or(&[])
}
fn task(length: usize) -> Json {
    let comments = (1..=length)
        .map(|at| {
            Json::object([
                (
                    "commentId",
                    if at == 12 {
                        "id3".into()
                    } else {
                        format!("id{at}").into()
                    },
                ),
                ("sequence", Json::Number(at as f64)),
                ("body", "Original \"\\\n😀".repeat(80).into()),
                (
                    "recipient",
                    Json::object([("kind", if at == 12 { "task" } else { "user" }.into())]),
                ),
                (
                    "opaque",
                    Json::String(JsString(vec![0xd800, 10, 65, 0xdfff])),
                ),
            ])
        })
        .collect();
    let receipts = (1..=length)
        .filter(|at| at % 3 == 0)
        .map(|at| {
            Json::object([
                (
                    "commentId",
                    if at == 12 {
                        "id3".into()
                    } else {
                        format!("id{at}").into()
                    },
                ),
                ("operation", format!("original-{at}").into()),
            ])
        })
        .collect();
    Json::object([
        ("comments", Json::Array(comments)),
        ("commentDeliveries", Json::Array(receipts)),
        (
            "fields",
            Json::object([("description", "Header".repeat(512).into())]),
        ),
        ("opaque", Json::String(JsString(vec![0xd800, 0, 0xdfff]))),
        ("commentStore", Json::Number(f64::NAN)),
    ])
}
// The original per-header/per-entry validator remains the wire/error oracle.
fn original_validation(task: &Json, task_id: &JsString) -> IvyResult<()> {
    let mut header = task.clone();
    header.insert("comments", Json::Array(vec![]));
    header.insert("commentDeliveries", Json::Array(vec![]));
    header.insert(
        "commentStore",
        Json::object([
            ("kind", "tree".into()),
            ("root", Json::Null),
            ("length", 0.0.into()),
        ]),
    );
    canonical(&header, JSON_OBJECT_CONTENT_BYTES - 1024)?;
    let mut receipts = std::collections::HashMap::new();
    for receipt in task
        .required("commentDeliveries")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        receipts.insert(
            receipt
                .required("commentId")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?,
            receipt,
        );
    }
    for comment in task
        .required("comments")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        let id = comment
            .required("commentId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        canonical(
            &Json::object([
                ("schemaVersion", 1.0.into()),
                ("taskId", Json::String(task_id.clone())),
                ("kind", "comment".into()),
                (
                    "entry",
                    Json::object([
                        ("comment", comment.clone()),
                        (
                            "receipt",
                            receipts.get(id).copied().cloned().unwrap_or(Json::Null),
                        ),
                    ]),
                ),
            ]),
            JSON_OBJECT_CONTENT_BYTES - 1024,
        )?;
    }
    Ok(())
}
fn check_validation(task: &Json) {
    assert_eq!(
        validate_task_content(task, &"task".into()).map_err(|e| e.to_wire()),
        original_validation(task, &"task".into()).map_err(|e| e.to_wire())
    );
}

// The former append/copy_with/increment sequence defines the projected value.
fn original_result_projection(task: &Json, comment: &Json, publication: &Json) -> IvyResult<Json> {
    let mut comment = comment.clone();
    comment.insert(
        "delivery",
        if f(f(task, "fields"), "userContact").is_str("ticket") {
            Json::Null
        } else {
            publication.clone()
        },
    );
    let mut comments = f(task, "comments")
        .as_array()
        .ok_or_else(IvyError::internal)?
        .to_vec();
    comments.push(comment);
    let mut changes = Json::object([
        ("publication", Json::Null),
        ("lastHistory", publication.clone()),
        ("comments", Json::Array(comments)),
    ])
    .into_object()
    .unwrap();
    let pairs = task.as_object().expect("insert requires an object");
    let mut fields = Vec::with_capacity(pairs.len() + changes.len());
    for (key, value) in pairs {
        let value = if let Some(at) = changes.iter().position(|(name, _)| name == key) {
            changes.remove(at).1
        } else {
            value.clone()
        };
        fields.push((key.clone(), value));
    }
    fields.extend(changes);
    let mut projected = Json::Object(fields);
    let count = projected
        .required("agentCommentCount")?
        .as_f64()
        .ok_or_else(IvyError::internal)?;
    projected.insert("agentCommentCount", Json::Number(count + 1.0));
    Ok(projected)
}

fn result_capacity(result: IvyResult<()>) -> IvyResult<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(error) if error.code == "content_too_large" => Ok(false),
        Err(error) => Err(error),
    }
}

#[test]
fn result_comment_projection_preserves_original_errors_and_raw_duplicate_keys() {
    let mut base = task(3);
    base.insert("agentCommentCount", 0.0.into());
    base.insert("publication", Json::Number(f64::NAN));
    base.insert("lastHistory", Json::Number(f64::NAN));
    let comment = Json::object([
        ("commentId", "id3".into()),
        (
            "body",
            Json::String(JsString(vec![0xd800, 0, 34, 92, 0xdfff])),
        ),
        ("delivery", Json::Number(f64::NAN)),
    ]);
    let publication = Json::object([("objectId", "original".into()), ("revision", 7.0.into())]);
    let check = |task: &Json, comment: &Json, publication: &Json| {
        let expected = original_result_projection(task, comment, publication)
            .and_then(|projected| result_capacity(original_validation(&projected, &"task".into())));
        assert_eq!(
            result_comment_fits(task, comment, publication, &"task".into())
                .map_err(|e| e.to_wire()),
            expected.map_err(|e| e.to_wire()),
        );
    };
    for contact in ["ticket", "chat", "voice"] {
        let mut value = base.clone();
        value
            .get_mut("fields")
            .unwrap()
            .insert("userContact", contact.into());
        check(&value, &comment, &publication);
    }
    let oversized = "x".repeat(JSON_OBJECT_CONTENT_BYTES);
    for (key, value) in [
        ("comments", Json::Null),
        (
            "comments",
            Json::Array(vec![Json::object([] as [(&str, Json); 0])]),
        ),
        ("commentDeliveries", Json::Null),
        (
            "commentDeliveries",
            Json::Array(vec![Json::object([("commentId", 1.0.into())])]),
        ),
        (
            "commentDeliveries",
            Json::Array(vec![Json::object([] as [(&str, Json); 0])]),
        ),
        (
            "commentDeliveries",
            Json::Array(vec![Json::object([
                ("commentId", "unselected".into()),
                ("opaque", Json::Number(f64::NAN)),
            ])]),
        ),
        ("opaque", Json::Number(f64::NAN)),
        ("opaque", oversized.clone().into()),
        ("agentCommentCount", Json::Null),
        ("agentCommentCount", Json::Number(f64::NAN)),
        ("agentCommentCount", Json::Number(f64::INFINITY)),
        ("agentCommentCount", 9_007_199_254_740_991.0.into()),
    ] {
        let mut value_task = base.clone();
        value_task.insert(key, value);
        check(&value_task, &comment, &publication);
        let mut missing = value_task.clone();
        missing.remove("agentCommentCount");
        check(&missing, &comment, &publication);
        value_task.insert("opaque", oversized.clone().into());
        check(&value_task, &comment, &publication);
    }
    for key in [
        "comments",
        "commentDeliveries",
        "publication",
        "lastHistory",
        "agentCommentCount",
    ] {
        let mut pairs = base.as_object().unwrap().to_vec();
        pairs.push((key.into(), Json::Number(f64::NAN)));
        check(&Json::Object(pairs), &comment, &publication);
    }
    for key in ["delivery", "commentId", "body"] {
        let mut pairs = comment.as_object().unwrap().to_vec();
        pairs.push((key.into(), Json::Number(f64::NAN)));
        check(&base, &Json::Object(pairs), &publication);
    }
    let mut missing_comment = comment.clone();
    missing_comment.remove("commentId");
    check(&base, &missing_comment, &publication);
    check(&base, &comment, &Json::Number(f64::NAN));
    let mut large_comment = comment.clone();
    large_comment.insert("body", oversized.into());
    check(&base, &large_comment, &publication);
    for key in ["opaque", "commentDeliveries"] {
        let mut invalid = base.clone();
        invalid.insert(key, Json::Number(f64::NAN));
        check(&invalid, &missing_comment, &publication);
        check(&invalid, &large_comment, &publication);
    }
}

#[test]
fn result_comment_capacity_borrows_header_and_history_without_a_combined_list() {
    if isolated("result_comment_capacity_borrows_header_and_history_without_a_combined_list") {
        return;
    }
    let comment = Json::object([("commentId", "generated".into()), ("body", "result".into())]);
    let publication = Json::object([("objectId", "original".into()), ("revision", 7.0.into())]);
    for length in [0, 64, 600] {
        let mut task = task(length);
        task.insert("agentCommentCount", 0.0.into());
        task.get_mut("fields")
            .unwrap()
            .insert("description", "header".repeat(120_000).into());
        let original = peak(|| {
            let projected = original_result_projection(&task, &comment, &publication).unwrap();
            // The previous append used the same bounded, borrowed validator.
            validate_task_content(&projected, &"task".into()).unwrap();
        });
        let borrowed = peak(|| {
            assert!(result_comment_fits(&task, &comment, &publication, &"task".into()).unwrap());
        });
        assert!(
            original > 1024 * 1024,
            "original projection peak: {original}"
        );
        assert!(
            borrowed < 128 * 1024,
            "result capacity copied header/history: {borrowed}"
        );
        eprintln!(
            "Result comment projection {length} comments: copied={original}, borrowed={borrowed} bytes beside owned input"
        );
    }
}
#[test]
fn content_bounds_preserve_original_errors_and_avoid_history_and_text_copies() {
    if isolated("content_bounds_preserve_original_errors_and_avoid_history_and_text_copies") {
        return;
    }
    for length in [0, 65, 300, 4096] {
        let task = task(length);
        check_validation(&task);
        let old = peak(|| {
            original_validation(&task, &"task".into()).unwrap();
        });
        let new = peak(|| {
            validate_task_content(&task, &"task".into()).unwrap();
        });
        assert!(new < 256 * 1024, "validation temporary peak: {new}");
        if length > 0 {
            assert!(old > length as isize * 1024, "original peak: {old}");
        }
        eprintln!(
            "Content validation {length} comments: original={old}, borrowed={new} bytes beside owned input"
        );
    }
    let base = task(3);
    for (key, value) in [
        ("opaque", Json::Number(f64::NAN)),
        ("opaque", "x".repeat(JSON_OBJECT_CONTENT_BYTES).into()),
        ("comments", Json::Null),
        ("commentDeliveries", Json::Null),
        (
            "commentDeliveries",
            Json::Array(vec![Json::object([("commentId", 1.0.into())])]),
        ),
        (
            "comments",
            Json::Array(vec![Json::object([
                ("commentId", "id".into()),
                ("body", "x".repeat(JSON_OBJECT_CONTENT_BYTES).into()),
            ])]),
        ),
    ] {
        let mut value_task = base.clone();
        value_task.insert(key, value);
        check_validation(&value_task);
    }
    let mut pairs = base.into_object().unwrap();
    pairs.push(("comments".into(), Json::Number(f64::NAN)));
    check_validation(&Json::Object(pairs));
}
// Original full view cloning defines ordering, optional fields and receipt selection.
fn original_page(view: &Json, query: &Json, open: Vec<Json>) -> Json {
    let task = view.required("task").unwrap();
    let all: Vec<_> = if f(f(query, "comments"), "questionsOnly").as_bool() == Some(true) {
        open.iter().collect()
    } else {
        array(f(task, "comments")).iter().collect()
    };
    let limit = f(f(query, "comments"), "limit")
        .as_f64()
        .map(|v| v as usize)
        .unwrap_or(50);
    let offset = f(f(query, "comments"), "offset")
        .as_f64()
        .map(|v| v as usize)
        .unwrap_or(all.len().saturating_sub(limit));
    let selected =
        &all[offset.min(all.len())..(offset + limit).min(all.len()).max(offset.min(all.len()))];
    let mut content = task.clone();
    content.insert(
        "comments",
        Json::Array(selected.iter().map(|c| (*c).clone()).collect()),
    );
    content.insert(
        "commentDeliveries",
        Json::Array(
            array(f(task, "commentDeliveries"))
                .iter()
                .filter(|r| {
                    selected
                        .iter()
                        .any(|c| f(c, "commentId") == f(r, "commentId"))
                })
                .cloned()
                .collect(),
        ),
    );
    let mut result = view.clone();
    result.insert("task", content);
    result.insert(
        "openQuestions",
        Json::Array(
            open.iter()
                .filter(|c| f(f(c, "recipient"), "kind") != &"task".into())
                .cloned()
                .collect(),
        ),
    );
    result.insert(
        "commentPage",
        Json::object([
            ("total", Json::Number(all.len() as f64)),
            ("offset", Json::Number(offset as f64)),
            (
                "nextOffset",
                if offset + selected.len() < all.len() {
                    Json::Number((offset + selected.len()) as f64)
                } else {
                    Json::Null
                },
            ),
        ]),
    );
    result
}
#[test]
fn fallback_pages_preserve_original_overlay_and_bound_temporary_memory() {
    if isolated("fallback_pages_preserve_original_overlay_and_bound_temporary_memory") {
        return;
    }
    for length in [0, 65, 300, 4096] {
        let mut task = task(length);
        task.insert("commentStore", Json::Null);
        let open: Vec<_> = array(f(&task, "comments"))
            .iter()
            .filter(|c| [3.0, 12.0].contains(&f(c, "sequence").as_f64().unwrap()))
            .cloned()
            .collect();
        let view = Json::object([
            ("task", task),
            (
                "openQuestions",
                Json::Array(vec!["replaced".repeat(100_000).into()]),
            ),
            ("commentPage", Json::Bool(false)),
            ("opaque", Json::String(JsString(vec![0xd800, 0]))),
        ]);
        let calls = Cell::new(0);
        let resolver = |_: &Json, _: Option<&JsString>| -> IvyResult<Vec<Json>> {
            calls.set(calls.get() + 1);
            Ok(open.clone())
        };
        let condition = |_: &Json, _: Option<&Json>| -> IvyResult<Json> { unreachable!() };
        let policies = ConversationPolicies {
            questions: &resolver,
            condition: &condition,
        };
        for questions in [false, true] {
            for (offset, limit) in [
                (None, None),
                (None, Some(0)),
                (Some(0), Some(1)),
                (Some(2), Some(7)),
                (Some(length + 1), Some(5)),
            ] {
                let mut options = Json::object([("questionsOnly", questions.into())]);
                if let Some(v) = offset {
                    options.insert("offset", Json::Number(v as f64));
                }
                if let Some(v) = limit {
                    options.insert("limit", Json::Number(v as f64));
                }
                let query = Json::object([("comments", options)]);
                let expected = original_page(&view, &query, open.clone());
                let page = task_view_page(&policies, &view, &query).unwrap();
                assert_eq!(calls.replace(0), 1);
                assert_eq!(page, expected);
                assert_eq!(
                    ivy_contracts::json::stringify(&page, usize::MAX).unwrap(),
                    ivy_contracts::json::stringify(&expected, usize::MAX).unwrap()
                );
            }
        }
        let query = Json::object([("comments", Json::object([("limit", 1.0.into())]))]);
        let mut duplicate_task = f(&view, "task").clone().into_object().unwrap();
        duplicate_task.push(("comments".into(), Json::Array(vec![Json::Null])));
        duplicate_task.push(("commentDeliveries".into(), Json::Array(vec![Json::Null])));
        let mut duplicate_view = view.clone();
        duplicate_view.insert("task", Json::Object(duplicate_task));
        let mut duplicate_view = duplicate_view.into_object().unwrap();
        duplicate_view.extend([
            ("task".into(), Json::Null),
            ("openQuestions".into(), Json::Bool(false)),
            ("commentPage".into(), Json::Bool(true)),
        ]);
        let duplicate_view = Json::Object(duplicate_view);
        let expected = original_page(&duplicate_view, &query, open.clone());
        let actual = task_view_page(&policies, &duplicate_view, &query).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(
            ivy_contracts::json::stringify(&actual, usize::MAX).unwrap(),
            ivy_contracts::json::stringify(&expected, usize::MAX).unwrap()
        );
        let old = peak(|| {
            drop(original_page(&view, &query, open.clone()));
        });
        let new = peak(|| {
            drop(task_view_page(&policies, &view, &query).unwrap());
        });
        assert!(new < 256 * 1024, "page temporary peak: {new}");
        assert!(
            old > 1024 * 1024,
            "original replaced openQuestions peak: {old}"
        );
        eprintln!(
            "Fallback page {length} comments: original={old}, selected={new} bytes including result"
        );
    }
}
