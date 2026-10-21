use super::*;
use ivy_hive::{
    store::Credential,
    worker_actions::WorkerActions,
    worker_kernel::KernelWorker,
    worker_protocol::{FrameReader, Message, Request, write_frame},
    worker_server::{self, BackupTask, Completion, Progress, WorkerService},
};
use rusqlite::{Connection, functions::FunctionFlags};
use std::{
    future::poll_fn,
    io::Write,
    process::{Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::Poll,
};
use tokio::{runtime::Builder, sync::Notify, time::timeout};

static CORPUS: Mutex<()> = Mutex::new(());
fn json_value(value: &IvyJson) -> Value {
    serde_json::from_str(&stringify(value, usize::MAX).unwrap()).unwrap()
}
fn original(input: &Value) -> Value {
    let mut child =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../tests/fixtures/hive-worker-loop-oracle.mjs"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn record(case: &str, value: Value) {
    if let Some(path) = std::env::var_os("IVY_HIVE_WORKER_KERNEL_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap(),
            "{}",
            json!({"case":case,"value":value})
        )
        .unwrap();
    }
}
fn service(root: &Root, fail: usize) -> (KernelWorker, Arc<AtomicUsize>) {
    // Reuse the original primitive profile and existing native kernel composer.
    let profile = oracle(
        &root.0.join("profile.sqlite"),
        &[json!({"op":"worker","method":"constructor","now":100000,"credentials":[]})],
    );
    let mut kernel = compose(
        &root.0.join("native.sqlite"),
        &profile["registryRuntime"],
        Rc::new(Cell::new(100000.0)),
    );
    let credentials = [Credential {
        principal_id: "alpha".into(),
        digest: format!("sha256:{}", "a".repeat(64)).into(),
    }];
    kernel
        .parts
        .store
        .configure_credentials(&credentials)
        .unwrap();
    let actions = WorkerActions::new(&mut kernel, &credentials).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    kernel
        .parts
        .store
        .connection()
        .create_scalar_function(
            "fixture_sequence",
            0,
            FunctionFlags::SQLITE_UTF8,
            move |_| {
                if observed.fetch_add(1, Ordering::SeqCst) < fail {
                    Err(rusqlite::Error::UserFunctionError(Box::new(
                        std::io::Error::other("Native sequence fixture failed"),
                    )))
                } else {
                    Ok(7_i64)
                }
            },
        )
        .unwrap();
    (KernelWorker::new(kernel, actions), calls)
}
fn credentials() -> Value {
    json!([{"principalId":"alpha","digest":format!("sha256:{}","a".repeat(64))}])
}
const VIEW: &str = "ALTER TABLE events RENAME TO saved_events; CREATE VIEW events AS SELECT fixture_sequence() AS sequence;";
fn dispatch_frames(service: &mut KernelWorker, id: u64, payload: &Value) -> (Vec<Value>, bool) {
    let dispatch = service.dispatch(IvyJson::from_serde(payload).unwrap());
    let mut frames = vec![json_value(&Message::Started(id).to_json())];
    frames.extend(
        dispatch
            .notifications
            .into_iter()
            .map(|v| json_value(&v.to_json())),
    );
    match dispatch.completion {
        Completion::Reply(result) => {
            frames.push(json_value(&Message::Reply { id, result }.to_json()));
            (frames, false)
        }
        Completion::Failed(_) => (frames, true),
    }
}

#[test]
fn kernel_worker_matches_original_publication_retry_action_errors_and_partial_fatal_chain() {
    for (fail, action_error) in [(0, false), (1, false), (2, false), (0, true), (1, true)] {
        let root = Root::new();
        let (mut worker, calls) = service(&root, fail);
        let failure = if action_error {
            json!({"action":"authenticate","context":{"credentialDigest":"invalid"}})
        } else {
            json!({"action":"health"})
        };
        let steps = json!([
            {"id":1,"payload":{"action":"health"}},
            {"id":2,"sql":VIEW,"invalidate":["objects","system/fixture"],"payload":failure},
            {"id":3,"payload":{"action":"health"}}
        ]);
        let expected =
            original(&json!({"credentials":credentials(),"failSequenceCalls":fail,"steps":steps}));
        let mut frames = vec![];
        let mut rejected = false;
        for step in steps.as_array().unwrap() {
            if let Some(sql) = step["sql"].as_str() {
                worker
                    .kernel()
                    .unwrap()
                    .parts
                    .store
                    .connection()
                    .execute_batch(sql)
                    .unwrap();
            }
            for scope in step["invalidate"].as_array().into_iter().flatten() {
                worker
                    .kernel_mut()
                    .unwrap()
                    .parts
                    .store
                    .invalidate(scope.as_str().unwrap().into());
            }
            let (values, failed) =
                dispatch_frames(&mut worker, step["id"].as_u64().unwrap(), &step["payload"]);
            frames.extend(values);
            rejected = failed;
            if failed {
                break;
            }
        }
        assert_eq!(
            json!(frames),
            expected["frames"],
            "fail={fail} action_error={action_error}"
        );
        assert_eq!(
            rejected,
            expected["states"].as_array().unwrap().last().unwrap()["rejected"].is_object()
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            expected["sequenceCalls"].as_u64().unwrap() as usize
        );
        record(
            "publication",
            json!({"fail":fail,"actionError":action_error,"original":expected,"nativeFrames":frames,"nativeRejected":rejected,"nativeSequenceCalls":calls.load(Ordering::SeqCst)}),
        );
    }
}

#[test]
fn kernel_worker_preserves_original_close_failure_and_releases_actual_sqlite_owner() {
    for changes in [false, true] {
        let root = Root::new();
        let (mut worker, _) = service(&root, 0);
        if changes {
            worker
                .kernel_mut()
                .unwrap()
                .parts
                .store
                .invalidate("before-close".into());
        }
        let expected = original(
            &json!({"credentials":credentials(),"steps":[{"id":1,"invalidate":if changes{json!(["before-close"])}else{json!([])},"payload":{"action":"close"}}]}),
        );
        let (frames, rejected) = dispatch_frames(&mut worker, 1, &json!({"action":"close"}));
        assert_eq!(json!(frames), expected["frames"]);
        assert!(rejected);
        assert_eq!(expected["databaseOpen"], false);
        assert_eq!(expected["portClosed"], false);
        assert!(worker.kernel().is_none());
        // The adapter remains alive, but its exclusive SQLite owner is closed.
        let database = Connection::open(root.0.join("native.sqlite")).unwrap();
        database
            .execute_batch("BEGIN EXCLUSIVE; CREATE TABLE after_close(value); COMMIT;")
            .unwrap();
        record(
            "close",
            json!({"changes":changes,"original":expected,"nativeFrames":frames,"nativeRejected":rejected,"exclusiveReopen":true}),
        );
    }
}

#[test]
fn kernel_worker_server_preserves_partial_publication_before_ending_without_reply() {
    let root = Root::new();
    let (mut worker, _) = service(&root, 2);
    worker
        .kernel()
        .unwrap()
        .parts
        .store
        .connection()
        .execute_batch(VIEW)
        .unwrap();
    worker
        .kernel_mut()
        .unwrap()
        .parts
        .store
        .invalidate("partial".into());
    let expected = original(
        &json!({"credentials":credentials(),"failSequenceCalls":2,"steps":[{"id":1,"sql":VIEW,"invalidate":["partial"],"payload":{"action":"health"}}]}),
    );
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (parent, child) = tokio::io::duplex(4096);
            let (reader, mut writer) = tokio::io::split(parent);
            let (input, output) = tokio::io::split(child);
            let server = worker_server::serve(input, output, |_| Ok(worker));
            let caller = async move {
                write_frame(&mut writer, &IvyJson::Null).await.unwrap();
                write_frame(
                    &mut writer,
                    &Request {
                        id: 1,
                        payload: IvyJson::object([("action", "health".into())]),
                    }
                    .to_json(),
                )
                .await
                .unwrap();
                let mut reader = FrameReader::new(reader);
                let mut frames = vec![];
                while let Some(value) = reader.next().await.unwrap() {
                    frames.push(json_value(&value));
                }
                frames
            };
            let (served, frames) = timeout(Duration::from_secs(10), async {
                tokio::join!(server, caller)
            })
            .await
            .unwrap();
            assert!(served.is_err());
            assert_eq!(frames[0], json!({"ready":true}));
            assert_eq!(json!(frames[1..]), expected["frames"]);
            record(
                "framed-fatal",
                json!({"original":expected,"nativeFrames":frames,"serverFailed":true}),
            );
        });
}

#[test]
fn kernel_worker_online_backup_shares_live_domains_rejects_overlap_and_finishes_before_close() {
    struct Held {
        worker: KernelWorker,
        gate: Arc<Notify>,
        paused: Rc<Cell<bool>>,
    }
    impl WorkerService for Held {
        fn handle(&mut self, p: IvyJson) -> IvyResult<IvyJson> {
            self.worker.handle(p)
        }
        fn notifications(&mut self) -> Vec<ivy_hive::worker_protocol::Notification> {
            self.worker.notifications()
        }
        fn dispatch(&mut self, p: IvyJson) -> worker_server::Dispatch {
            self.worker.dispatch(p)
        }
        fn backup(&mut self, p: IvyJson, progress: Progress) -> IvyResult<BackupTask> {
            let mut task = self.worker.backup(p, progress)?;
            let gate = self.gate.clone();
            let paused = self.paused.clone();
            Ok(Box::pin(async move {
                match poll_fn(|cx| Poll::Ready(task.as_mut().poll(cx))).await {
                    Poll::Ready(result) => return result,
                    Poll::Pending => {
                        paused.set(true);
                        gate.notified().await;
                    }
                }
                task.await
            }))
        }
    }
    let root = Root::new();
    let (worker, _) = service(&root, 0);
    worker.kernel().unwrap().parts.store.connection().execute_batch("CREATE TABLE backup_payload(value BLOB); WITH RECURSIVE n(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<512) INSERT INTO backup_payload SELECT zeroblob(32768) FROM n;").unwrap();
    let backup = root.0.join("snapshot.sqlite");
    let snapshot = backup.clone();
    let paused = Rc::new(Cell::new(false));
    let gate = Arc::new(Notify::new());
    let release = gate.clone();
    let held = Held {
        worker,
        gate,
        paused: paused.clone(),
    };
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async move {
        let (parent,child)=tokio::io::duplex(65536);let (reader,mut writer)=tokio::io::split(parent);let (input,output)=tokio::io::split(child);
        let server=worker_server::serve(input,output,|_|Ok(held));
        let caller=async move {
            write_frame(&mut writer,&IvyJson::Null).await.unwrap();
            let destination=backup.to_str().unwrap();let changed=format!("sha256:{}","b".repeat(64));
            for (id,payload) in [
                (1,json!({"action":"backup","destination":destination})),
                (2,json!({"action":"health"})),
                (3,json!({"action":"backup","destination":destination})),
                (4,json!({"action":"credentials.configure","credentials":[{"principalId":"alpha","digest":changed}]})),
                (5,json!({"action":"authenticate","context":{"credentialDigest":format!("sha256:{}","a".repeat(64))}})),
                (6,json!({"action":"authenticate","context":{"credentialDigest":changed}})),
                (7,json!({"action":"close"})),
            ]{write_frame(&mut writer,&Request{id,payload:IvyJson::from_serde(&payload).unwrap()}.to_json()).await.unwrap();}
            let mut reader=FrameReader::new(reader);let mut frames=vec![];
            while let Some(value)=reader.next().await.unwrap(){let value=json_value(&value);if value==json!({"id":"7","started":true}){assert!(paused.get(),"The actual copy must have yielded before close");release.notify_one();}frames.push(value);}frames
        };
        let (served,frames)=timeout(Duration::from_secs(30),async{tokio::join!(server,caller)}).await.unwrap();assert!(served.is_err(),"Original close publication rejects after backup completion");
        let position=|id:&str,key:&str|frames.iter().position(|v|v["id"]==id&&v.get(key).is_some()).unwrap();
        let reply=|id:&str|frames.iter().find(|v|v["id"]==id&&(v.get("value").is_some()||v.get("error").is_some())).unwrap();
        assert_eq!(reply("2")["value"],json!({"ready":true}));assert_eq!(reply("3")["error"]["data"]["code"],"limit_exceeded");assert_eq!(reply("5")["error"]["data"]["code"],"unauthenticated");assert_eq!(reply("6")["value"]["principalId"],"alpha");
        assert!(position("7","started")<position("1","value"));assert!(position("1","progress")<position("1","value"));assert!(!frames.iter().any(|v|v["id"]=="7"&&v.get("value").is_some()));
        let saved=Connection::open(&snapshot).unwrap();assert_eq!(saved.query_row("PRAGMA integrity_check",[],|r|r.get::<_,String>(0)).unwrap(),"ok");assert_eq!(saved.query_row("SELECT COUNT(*) FROM backup_payload",[],|r|r.get::<_,i64>(0)).unwrap(),512);assert_eq!(saved.query_row("SELECT revoked FROM credentials WHERE digest=?",[format!("sha256:{}","a".repeat(64))],|r|r.get::<_,i64>(0)).unwrap(),1);
        assert_eq!(reply("1")["value"]["bytesHash"],ivy_contracts::hash::digest(&fs::read(&snapshot).unwrap()));
        record("framed-backup",json!({"nativeFrames":frames,"serverFailedAtOriginalClose":true,"snapshotIntegrity":true,"liveCredentialMutationIncluded":true}));
    });
}
