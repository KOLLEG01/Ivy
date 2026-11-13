use super::{Limits, memory, value};
use ivy_contracts::IvyJson;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicIsize, Ordering},
};

struct Counted;
static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);
fn added(bytes: isize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}
unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            added(layout.size() as isize);
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            added(layout.size() as isize);
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let next = unsafe { System.realloc(pointer, layout, size) };
        if !next.is_null() {
            added(size as isize - layout.size() as isize);
        }
        next
    }
}
#[global_allocator]
static ALLOCATOR: Counted = Counted;

#[test]
fn reopening_compacts_duplicated_heads_without_retaining_the_bodies() {
    const CHILD: &str = "IVY_SECRETARY_HEAD_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "retention_memory::reopening_compacts_duplicated_heads_without_retaining_the_bodies", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        return;
    }
    let directory = super::Scratch(
        std::env::temp_dir().join(format!("secretary-head-memory-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&directory.0).unwrap();
    let path = directory.0.join("journal.sqlite");
    let journal = super::Journal::open(&path, Limits::default()).unwrap();
    let mut original = value(r#"{"phase":"succeeded"}"#);
    original.insert("body", "x".repeat(128 * 1024).into());
    for index in 0..48 {
        journal
            .create_technical("workflow", &format!("record-{index}"), &original)
            .unwrap();
    }
    journal.db.execute("UPDATE technical_documents SET value_json=(SELECT r.value_json FROM technical_revisions r WHERE r.id=technical_documents.id AND r.revision=technical_documents.current_revision)", []).unwrap();
    let original_usage = journal.usage().unwrap();
    drop(original);
    drop(journal);
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let journal = super::Journal::open(&path, Limits::default()).unwrap();
    let extra = PEAK.load(Ordering::Relaxed) - before;
    assert!(
        extra < 2 * 1024 * 1024,
        "Compaction retained original bodies: {extra}"
    );
    assert_eq!(journal.usage().unwrap(), original_usage);
    let heads: i64 = journal
        .db
        .query_row(
            "SELECT SUM(length(CAST(value_json AS BLOB))) FROM technical_documents",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(heads < 2_048);
    eprintln!(
        "Secretary head rebuild Rust allocation peak above baseline: {extra} bytes; stored heads {heads} bytes; original revisions {} bytes for 48 x 128 KiB bodies",
        original_usage.bytes
    );
}

#[test]
fn cleanup_drops_record_bodies_between_rows() {
    const CHILD: &str = "IVY_SECRETARY_RETENTION_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "retention_memory::cleanup_drops_record_bodies_between_rows",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        if output.status.success() {
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        }
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let mut journal = memory(Limits::default());
    let mut document = value(r#"{"phase":"succeeded"}"#);
    document.insert("body", IvyJson::from("x".repeat(128 * 1024)));
    for index in 0..48 {
        let saved = journal
            .create_technical("workflow", &format!("record-{index}"), &document)
            .unwrap();
        journal
            .complete_technical_workflow(&saved.pin.object_id, 0)
            .unwrap();
    }
    let (heads, originals): (i64, i64) = journal.db.query_row(
        "SELECT (SELECT SUM(length(CAST(value_json AS BLOB))) FROM technical_documents),(SELECT SUM(length(CAST(value_json AS BLOB))) FROM technical_revisions)",
        [], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
    assert!(heads < 2_048, "Heads duplicated payloads: {heads}");
    assert!(originals > 48 * 128 * 1024);
    assert_eq!(journal.usage().unwrap().bytes, originals);
    drop(document);
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    journal.limits.count = 0;
    journal.cleanup(0, 0, None, None).unwrap();
    let extra = PEAK.load(Ordering::Relaxed) - before;
    eprintln!(
        "Secretary cleanup Rust allocation peak above baseline: {extra} bytes for 48 x 128 KiB bodies"
    );
    // Count actual Rust allocator requests, independently of SQLite's native heap or RSS.
    assert!(
        extra < 2 * 1024 * 1024,
        "Cleanup retained the record bodies: {extra} bytes"
    );
    assert_eq!(journal.usage().unwrap().count, 0);
}

#[test]
fn store_admits_owned_documents_without_another_body_or_encoded_buffer() {
    const CHILD: &str = "IVY_SECRETARY_STORE_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "retention_memory::store_admits_owned_documents_without_another_body_or_encoded_buffer",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        return;
    }
    use ivy_contracts::{
        IvyResult, JsString,
        hash::hash_json,
        json::{canonical_size, parse},
    };
    use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
    use ivy_secretary::{
        schema::{Schema, validate},
        store::{Record, Store, record_references},
    };
    use std::{io::Read, sync::Arc};
    struct NoRpc;
    impl RpcClient for NoRpc {
        fn request<'a>(&'a self, _: &'a str, _: IvyJson, _: RequestOptions) -> RequestFuture<'a> {
            Box::pin(async { panic!("Document admission must not issue an RPC") })
        }
    }
    let mut text = String::new();
    flate2::read::GzDecoder::new(&include_bytes!("store-cases.json.gz")[..])
        .read_to_string(&mut text)
        .unwrap();
    let corpus = parse(&text, 16 * 1024 * 1024).unwrap();
    let mut raw = corpus
        .required("rows")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find_map(|r| {
            let input = r.get("input")?;
            let args = input.get("args")?.as_array()?;
            (input.get("method")?.is_str("decode")
                && args.first()?.is_str("secretary/execution")
                && r.get("expected")?.get("result")?.get("value").is_some())
            .then(|| args[1].clone())
        })
        .unwrap();
    let body = raw.get_mut("content").unwrap().get_mut("value").unwrap();
    body.get_mut("trigger").unwrap().insert(
        "payload",
        IvyJson::object([
            (
                "body",
                IvyJson::String(JsString(vec![b'x' as u16; 896 * 1024])),
            ),
            ("units", IvyJson::String(JsString(vec![0xd800, 0, 0xdc00]))),
        ]),
    );
    // Warm the shared schema bundle before measuring document admission.
    validate(Schema::Execution, body).unwrap();
    let hash = hash_json(body).unwrap();
    let bytes = canonical_size(body, 1_048_576).unwrap();
    let refs = record_references(Record::Execution, body).unwrap();
    let metadata = raw.get_mut("revision").unwrap();
    metadata.insert("contentHash", hash.clone().into());
    metadata.insert("byteLength", (bytes as f64).into());
    metadata.insert("references", refs);
    let journal = memory(Limits::default());
    let store = Store {
        client: Arc::new(NoRpc),
        root: corpus
            .required("root")
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone(),
        journal: &journal,
    };
    drop(corpus);
    drop(text);
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let admitted: IvyResult<IvyJson> = store.decode(Record::Execution, raw);
    let extra = PEAK.load(Ordering::Relaxed) - before;
    let admitted = admitted.unwrap();
    eprintln!(
        "Secretary owned document Rust allocation peak above baseline: {extra} bytes for {bytes} canonical bytes"
    );
    assert_eq!(
        hash_json(admitted.required("value").unwrap()).unwrap(),
        hash
    );
    assert_eq!(
        admitted
            .required("value")
            .unwrap()
            .required("trigger")
            .unwrap()
            .required("payload")
            .unwrap()
            .get("units"),
        Some(&IvyJson::String(JsString(vec![0xd800, 0, 0xdc00])))
    );
    assert!(
        extra < 256 * 1024,
        "Document admission copied the body or encoding: {extra} bytes"
    );
}
