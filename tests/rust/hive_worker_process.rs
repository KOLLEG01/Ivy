use ivy_contracts::{IvyError, IvyJson, JsString, Outcome, json::stringify};
use ivy_hive::{
    worker_client::{Lifecycle, MAX_PENDING, RequestOptions},
    worker_protocol::{FrameReader, MAX_FRAME_BYTES, Message, Request, write_frame},
    worker_runtime::{Event, StorageClient, WorkerLaunch},
};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    runtime::Builder,
    time::{sleep, timeout},
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ivy-hive-pipe-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        assert!(self.0.starts_with(std::env::temp_dir()));
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn launch(root: &Root) -> WorkerLaunch {
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_ivy-hive-worker-fixture"));
    WorkerLaunch {
        root: root.0.clone(),
        executable,
        args: vec![],
        options: IvyJson::object([("root", root.0.to_str().unwrap().into())]),
        job_launcher: if cfg!(windows) {
            Some(PathBuf::from(
                std::env::var_os("IVY_HIVE_TEST_JOB").unwrap_or_else(|| {
                    let target = std::env::var_os("CARGO_TARGET_DIR")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| {
                            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../target")
                        });
                    target.join("debug/ivy-job.exe").into_os_string()
                }),
            ))
        } else {
            None
        },
        environment: BTreeMap::new(),
    }
}
fn client(root: &Root) -> (StorageClient, Arc<Mutex<Vec<Event>>>) {
    let events = Arc::new(Mutex::new(vec![]));
    let sink = events.clone();
    (
        StorageClient::new(
            launch(root),
            Arc::new(move |event| sink.lock().unwrap().push(event)),
        ),
        events,
    )
}
fn payload(action: &str) -> IvyJson {
    IvyJson::object([("action", action.into())])
}
async fn wait_file(path: PathBuf) {
    timeout(Duration::from_secs(10), async {
        while !path.is_file() {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn stopped(client: &StorageClient) {
    timeout(Duration::from_secs(5), async {
        while client.lifecycle() != Lifecycle::Stopped {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Database owner not confirmed reaped");
}
async fn request(client: &StorageClient, value: IvyJson) -> Result<IvyJson, IvyError> {
    timeout(
        Duration::from_secs(10),
        client.request(value, RequestOptions::default()),
    )
    .await
    .unwrap()
    .unwrap()
}

#[test]
fn private_frames_survive_cancelled_partial_reads_and_preserve_full_u64_utf16_errors() {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let value =
                IvyJson::object([("x", IvyJson::String(JsString(vec![0xd800, 10, 0xdc00])))]);
            let encoded = stringify(&value, MAX_FRAME_BYTES).unwrap().into_bytes();
            let header = (encoded.len() as u32).to_be_bytes();
            let (mut input, output) = tokio::io::duplex(3);
            let expected = value.clone();
            let writer = tokio::spawn(async move {
                input.write_all(&header[..2]).await.unwrap();
                sleep(Duration::from_millis(20)).await;
                input.write_all(&header[2..]).await.unwrap();
                input.write_all(&encoded[..2]).await.unwrap();
                sleep(Duration::from_millis(20)).await;
                input.write_all(&encoded[2..]).await.unwrap();
                write_frame(
                    &mut input,
                    &Request {
                        id: u64::MAX,
                        payload: expected,
                    }
                    .to_json(),
                )
                .await
                .unwrap();
            });
            let mut reader = FrameReader::new(output);
            assert!(
                timeout(Duration::from_millis(5), reader.next())
                    .await
                    .is_err()
            );
            assert!(
                timeout(Duration::from_millis(25), reader.next())
                    .await
                    .is_err()
            );
            assert_eq!(reader.next().await.unwrap().unwrap(), value);
            let received = Request::from_json(reader.next().await.unwrap().unwrap()).unwrap();
            assert_eq!(received.id, u64::MAX);
            assert_eq!(received.payload, value);
            assert!(reader.next().await.unwrap().is_none());
            writer.await.unwrap();
            let error = IvyError::new_js(JsString(vec![0xd800]), JsString(vec![0xdc00]))
                .with_outcome(Outcome::Completed)
                .with_details(value);
            match Message::from_json(
                Message::Reply {
                    id: u64::MAX,
                    result: Err(error.clone()),
                }
                .to_json(),
            )
            .unwrap()
            .unwrap()
            {
                Message::Reply {
                    id,
                    result: Err(received),
                } => {
                    assert_eq!(id, u64::MAX);
                    assert_eq!(received.to_wire(), error.to_wire());
                }
                _ => panic!("Private error lost"),
            }
        });
}

#[test]
fn private_frames_reject_invalid_lengths_utf8_json_and_truncation_without_allocating_declared_garbage()
 {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            for (bytes, kind) in [
                (vec![0, 0, 0, 0], std::io::ErrorKind::InvalidData),
                (
                    u32::MAX.to_be_bytes().to_vec(),
                    std::io::ErrorKind::InvalidData,
                ),
                (vec![0, 0], std::io::ErrorKind::UnexpectedEof),
                (vec![0, 0, 0, 2, b'{'], std::io::ErrorKind::UnexpectedEof),
                (vec![0, 0, 0, 1, 255], std::io::ErrorKind::InvalidData),
                (vec![0, 0, 0, 1, b'x'], std::io::ErrorKind::InvalidData),
            ] {
                let mut reader = FrameReader::new(bytes.as_slice());
                assert_eq!(reader.next().await.unwrap_err().kind(), kind);
            }
        });
}

#[test]
fn completed_backup_reports_its_last_progress_before_reply_even_after_sender_closes() {
    use ivy_hive::worker_server::{self, BackupTask, Progress, WorkerService};
    struct Service(bool);
    impl WorkerService for Service {
        fn handle(&mut self, _: IvyJson) -> Result<IvyJson, IvyError> {
            Ok(true.into())
        }
        fn backup(&mut self, _: IvyJson, progress: Progress) -> Result<BackupTask, IvyError> {
            let delayed = self.0;
            Ok(Box::pin(async move {
                if delayed {
                    sleep(Duration::from_millis(5)).await;
                }
                progress();
                Ok(true.into())
            }))
        }
        fn notifications(&mut self) -> Vec<ivy_hive::worker_protocol::Notification> {
            vec![]
        }
    }
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            // The first case completes in the request loop, the second inside Close.
            for delayed in [false, true] {
                let (parent, child) = tokio::io::duplex(1024);
                let (input, mut output) = tokio::io::split(parent);
                let (reader, writer) = tokio::io::split(child);
                let server = worker_server::serve(reader, writer, |_| Ok(Service(delayed)));
                let caller = async move {
                    write_frame(&mut output, &IvyJson::Null).await.unwrap();
                    for (id, action) in [(1, "backup"), (2, "close")] {
                        write_frame(
                            &mut output,
                            &Request {
                                id,
                                payload: payload(action),
                            }
                            .to_json(),
                        )
                        .await
                        .unwrap();
                    }
                    let mut input = FrameReader::new(input);
                    let mut received = vec![];
                    while let Some(value) = input.next().await.unwrap() {
                        received.push(value);
                    }
                    let ready = Message::Ready.to_json();
                    let started = |id| Message::Started(id).to_json();
                    let progress = Message::Progress(1).to_json();
                    let reply = |id| {
                        Message::Reply {
                            id,
                            result: Ok(true.into()),
                        }
                        .to_json()
                    };
                    let expected = if delayed {
                        vec![ready, started(1), started(2), progress, reply(1), reply(2)]
                    } else {
                        vec![ready, started(1), progress, reply(1), started(2), reply(2)]
                    };
                    assert_eq!(received, expected);
                };
                let (served, ()) = tokio::join!(server, caller);
                served.unwrap();
            }
        });
}

#[test]
fn real_storage_child_shared_start_notifications_errors_and_close_reopen() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (client, events) = client(&root);
            // Closing an absent owner must not leave a delayed shutdown capable
            // of retiring the next explicit start.
            client.close().wait().await.unwrap();
            drop(client.start());
            let shared = client.start();
            timeout(Duration::from_secs(10), shared.wait())
                .await
                .unwrap()
                .unwrap();
            let value = IvyJson::String(JsString(vec![0xd800, 65, 0xdc00]));
            let result = request(
                &client,
                IvyJson::object([("action", "echo".into()), ("value", value.clone())]),
            )
            .await
            .unwrap();
            assert_eq!(result, value);
            assert_eq!(events.lock().unwrap().len(), 3);
            let error = request(
                &client,
                IvyJson::object([("action", "error".into()), ("value", value.clone())]),
            )
            .await
            .unwrap_err();
            assert_eq!(error.outcome, Outcome::Completed);
            assert_eq!(error.message_js(), JsString(vec![0xd800, 120, 0xdc00]));
            assert_eq!(error.details, Some(value));
            let a = client.close();
            let b = client.close();
            a.wait().await.unwrap();
            b.wait().await.unwrap();
            assert_eq!(client.lifecycle(), Lifecycle::Stopped);
            assert_eq!(client.pending_count(), 0);
            client.start().wait().await.unwrap();
            assert_eq!(
                request(&client, payload("health"))
                    .await
                    .unwrap()
                    .get("ready"),
                Some(&true.into())
            );
            client.close().wait().await.unwrap();
            assert!(
                !events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| matches!(e, Event::Failure(_)))
            );
        });
}

#[test]
fn real_storage_child_queue_timeout_and_dropped_receiver_keep_posted_work_until_reply() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (client, events) = client(&root);
            client.start().wait().await.unwrap();
            let active = client.request(payload("hold"), RequestOptions::default());
            wait_file(root.0.join("blocked")).await;
            let queued = client.request(
                payload("write"),
                RequestOptions {
                    timeout_ms: 30,
                    ..Default::default()
                },
            );
            let dropped = client.request(payload("write"), RequestOptions::default());
            drop(dropped);
            let error = queued.await.unwrap().unwrap_err();
            assert_eq!(error.outcome, Outcome::Unknown);
            assert_eq!(
                error.message,
                "Hive storage queue wait expired; reconcile operation identity."
            );
            assert_eq!(client.pending_count(), 3);
            assert_eq!(client.queued_bytes(), 12288);
            assert!(matches!(client.lifecycle(), Lifecycle::Ready(_)));
            fs::write(root.0.join("release"), b"go").unwrap();
            active.await.unwrap().unwrap();
            assert_eq!(
                request(&client, payload("calls")).await.unwrap(),
                IvyJson::from("2")
            );
            assert_eq!(client.pending_count(), 0);
            assert_eq!(client.queued_bytes(), 0);
            client.close().wait().await.unwrap();
            assert!(
                !events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| matches!(e, Event::Failure(_)))
            );
        });
}

#[test]
fn real_nonterminating_started_read_timeout_retains_its_slot_and_accepts_late_reply() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (client, events) = client(&root);
            client.start().wait().await.unwrap();
            let held = client.request(
                payload("hold"),
                RequestOptions {
                    timeout_ms: 100,
                    terminate_on_timeout: false,
                    ..Default::default()
                },
            );
            wait_file(root.0.join("blocked")).await;
            let error = held.await.unwrap().unwrap_err();
            assert_eq!(error.outcome, Outcome::Unknown);
            assert_eq!(
                error.message,
                "Hive storage response deadline exceeded; reconcile operation identity."
            );
            assert!(matches!(client.lifecycle(), Lifecycle::Ready(_)));
            assert_eq!(client.pending_count(), 1);
            assert_eq!(client.queued_bytes(), 4096);
            fs::write(root.0.join("release"), b"go").unwrap();
            request(&client, payload("health")).await.unwrap();
            assert_eq!(client.pending_count(), 0);
            assert_eq!(client.queued_bytes(), 0);
            client.close().wait().await.unwrap();
            assert!(
                events
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|e| !matches!(e, Event::Failure(_)))
            );
        });
}

#[test]
fn close_during_real_child_initialization_settles_start_only_after_fencing_the_owner() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut launch = launch(&root);
            launch.options.insert("neverReady", true.into());
            let client = StorageClient::new(launch, Arc::new(|_| {}));
            let starting = client.start();
            wait_file(root.0.join("pid")).await;
            let closing = client.close();
            let error = starting.wait().await.unwrap_err();
            assert_eq!(error.code, "service_unavailable");
            assert_eq!(error.outcome, Outcome::Unknown);
            timeout(Duration::from_secs(5), closing.wait())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(client.lifecycle(), Lifecycle::Stopped);
            // Opening the same path proves the actual retained SQLite owner is gone.
            ivy_hive::store::Store::open(&root.0.join("hive.sqlite")).unwrap();
        });
}

#[test]
fn real_started_timeout_reaps_blocked_sqlite_writer_before_explicit_restart_without_replay() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (client, events) = client(&root);
            client.start().wait().await.unwrap();
            let held = client.request(
                payload("hold"),
                RequestOptions {
                    timeout_ms: 150,
                    ..Default::default()
                },
            );
            wait_file(root.0.join("blocked")).await;
            let error = held.await.unwrap().unwrap_err();
            assert_eq!(error.outcome, Outcome::Unknown);
            assert_eq!(
                error.message,
                "Hive storage response deadline exceeded; reconcile operation identity."
            );
            let attempted = client.start();
            // Whether native retirement won the race or is still in progress, the
            // next ready owner can open the exclusive DB only after definite death.
            let result = attempted.wait().await;
            if result.is_err() {
                stopped(&client).await;
                client.start().wait().await.unwrap();
            }
            assert_eq!(
                request(&client, payload("calls")).await.unwrap(),
                IvyJson::from("0")
            );
            assert!(
                events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| matches!(e, Event::Failure(_)))
            );
            client.close().wait().await.unwrap();
            let db = rusqlite::Connection::open(root.0.join("hive.sqlite")).unwrap();
            assert_eq!(
                db.query_row("SELECT value FROM metadata WHERE key='posted'", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
                "yes"
            );
        });
}

#[test]
fn real_storage_child_bounded_backpressure_frees_all_reservations_only_after_retirement() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (client, _) = client(&root);
            client.start().wait().await.unwrap();
            let mut receivers = vec![client.request(
                payload("hold"),
                RequestOptions {
                    timeout_ms: 60_000,
                    ..Default::default()
                },
            )];
            wait_file(root.0.join("blocked")).await;
            for _ in 1..MAX_PENDING {
                receivers.push(client.request(
                    IvyJson::object([
                        ("action", "echo".into()),
                        ("value", "x".repeat(8192).into()),
                    ]),
                    RequestOptions {
                        timeout_ms: 60_000,
                        ..Default::default()
                    },
                ));
            }
            let refused = client
                .request(payload("health"), RequestOptions::default())
                .await
                .unwrap()
                .unwrap_err();
            assert_eq!(refused.code, "limit_exceeded");
            assert_eq!(refused.outcome, Outcome::NotExecuted);
            assert_eq!(client.pending_count(), MAX_PENDING);
            assert_eq!(client.queued_bytes(), MAX_PENDING as u64 * 4096);
            let close = client.close();
            for receiver in receivers {
                assert_eq!(
                    receiver.await.unwrap().unwrap_err().outcome,
                    Outcome::Unknown
                );
            }
            close.wait().await.unwrap();
            assert_eq!(client.pending_count(), 0);
            assert_eq!(client.queued_bytes(), 0);
        });
}

#[test]
fn real_storage_child_startup_deadline_and_fatal_preserve_failure_and_owning_retirement() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut launch = launch(&root);
            launch.options.insert("neverReady", true.into());
            let client = StorageClient::new(launch, Arc::new(|_| {}));
            let starting = client.start();
            // Options can reach the child only after the Host launch handshake and
            // pipe binding. Pause only the policy deadline; resume for real OS reap.
            wait_file(root.0.join("pid")).await;
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(30)).await;
            for _ in 0..32 {
                tokio::task::yield_now().await;
            }
            tokio::time::resume();
            let error = starting.wait().await.unwrap_err();
            assert_eq!(error.code, "deadline_exceeded");
            assert_eq!(error.outcome, Outcome::NotExecuted);
            stopped(&client).await;
            assert_eq!(client.pending_count(), 0);
            client.close().wait().await.unwrap();
        });
    let root = Root::new();
    let db = rusqlite::Connection::open(root.0.join("hive.sqlite")).unwrap();
    db.execute_batch("PRAGMA user_version=9").unwrap();
    drop(db);
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (client, _) = client(&root);
            let error = client.start().wait().await.unwrap_err();
            assert_eq!(error.code, "unsupported_storage_format");
            assert_eq!(error.outcome, Outcome::NotExecuted);
            stopped(&client).await;
            assert_eq!(
                rusqlite::Connection::open(root.0.join("hive.sqlite"))
                    .unwrap()
                    .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                9
            );
        });
}

#[test]
fn real_worker_backup_interleaves_committed_writes_rejects_second_backup_and_close_waits() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (client, _) = client(&root);
            client.start().wait().await.unwrap();
            let destination = root.0.join("copy.sqlite");
            let backup_payload = IvyJson::object([
                ("action", "backup".into()),
                ("destination", destination.to_str().unwrap().into()),
                ("delayMs", IvyJson::Number(3000.0)),
            ]);
            let backup = client.request(
                backup_payload.clone(),
                RequestOptions {
                    // The original progress throttle is 1000ms. Allow native I/O
                    // scheduling margin while still requiring a deadline reset.
                    timeout_ms: 2500,
                    ..Default::default()
                },
            );
            let second = client.request(backup_payload, RequestOptions::default());
            assert_eq!(
                second.await.unwrap().unwrap_err().message,
                "A storage backup is already active."
            );
            assert_eq!(
                request(&client, payload("write")).await.unwrap(),
                IvyJson::from("1")
            );
            let close = client.close();
            let pages = backup.await.unwrap().unwrap();
            assert!(pages.get("pages").unwrap().as_f64().unwrap() > 0.0);
            close.wait().await.unwrap();
            assert_eq!(
                pages.get("bytesHash"),
                Some(&ivy_contracts::hash::digest(fs::read(&destination).unwrap()).into())
            );
            let copy = rusqlite::Connection::open(destination).unwrap();
            assert_eq!(
                copy.query_row("SELECT value FROM metadata WHERE key='calls'", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
                "1"
            );
        });
}

#[test]
fn real_storage_child_malformed_frame_and_unexpected_exit_never_prove_not_executed() {
    for action in ["malformed", "exit"] {
        let root = Root::new();
        Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let (client, _) = client(&root);
                client.start().wait().await.unwrap();
                let error = request(&client, payload(action)).await.unwrap_err();
                assert_eq!(error.outcome, Outcome::Unknown);
                assert_eq!(error.code, "storage_unavailable");
                stopped(&client).await;
                client.start().wait().await.unwrap();
                request(&client, payload("health")).await.unwrap();
                client.close().wait().await.unwrap();
            });
    }
}

#[test]
fn concurrent_failure_close_and_explicit_restart_keep_settlements_in_their_generation() {
    let root = Root::new();
    Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (client, _) = client(&root);
            for _ in 0..8 {
                client.start().wait().await.unwrap();
                let _ = fs::remove_file(root.0.join("blocked"));
                let held = client.request(
                    payload("hold"),
                    RequestOptions {
                        timeout_ms: 150,
                        ..Default::default()
                    },
                );
                wait_file(root.0.join("blocked")).await;
                let queued = (0..8)
                    .map(|_| client.request(payload("write"), RequestOptions::default()))
                    .collect::<Vec<_>>();
                let closing = client.close();
                let replacement = client.clone();
                let restarting = tokio::spawn(async move {
                    stopped(&replacement).await;
                    timeout(Duration::from_secs(10), replacement.start().wait())
                        .await
                        .unwrap()
                        .unwrap();
                });
                assert_eq!(
                    held.await
                        .expect("Lost running settlement")
                        .unwrap_err()
                        .outcome,
                    Outcome::Unknown
                );
                for receiver in queued {
                    assert_eq!(
                        receiver
                            .await
                            .expect("Lost queued settlement")
                            .unwrap_err()
                            .outcome,
                        Outcome::Unknown
                    );
                }
                timeout(Duration::from_secs(5), closing.wait())
                    .await
                    .unwrap()
                    .unwrap();
                restarting.await.unwrap();
                assert_eq!(
                    request(&client, payload("calls")).await.unwrap(),
                    IvyJson::from("0")
                );
                client.close().wait().await.unwrap();
                assert_eq!(client.pending_count(), 0);
                assert_eq!(client.queued_bytes(), 0);
            }
        });
}

#[test]
fn native_launch_errors_preserve_worker_failure_and_require_explicit_absence_proof() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut launch = launch(&root);
            // A directory is an explicit Host NotExecuted rejection. The adapter
            // preserves the original worker error while accepting absence proof.
            launch.executable = root.0.clone();
            let client = StorageClient::new(launch.clone(), Arc::new(|_| {}));
            for _ in 0..2 {
                let error = client.start().wait().await.unwrap_err();
                assert_eq!(error.code, "storage_unavailable");
                assert_eq!(error.message, "Hive storage worker failed.");
                assert_eq!(error.outcome, Outcome::Unknown);
                assert_eq!(client.lifecycle(), Lifecycle::Stopped);
                assert!(!root.0.join("pid").exists());
                assert!(!root.0.join("hive.sqlite").exists());
            }
            client.close().wait().await.unwrap();
            // Pre-spawn filesystem errors currently carry Unknown in Host. Even
            // this known test path's absence is not a retirement proof to use.
            launch.executable = root.0.join("absent-worker.exe");
            let client = StorageClient::new(launch, Arc::new(|_| {}));
            let error = client.start().wait().await.unwrap_err();
            assert_eq!(error.code, "storage_unavailable");
            assert_eq!(error.message, "Hive storage worker failed.");
            assert_eq!(error.outcome, Outcome::Unknown);
            assert!(matches!(client.lifecycle(), Lifecycle::StopUnconfirmed(_)));
            let retry = client.start().wait().await.unwrap_err();
            assert_eq!(retry.message, "Hive storage is not ready.");
            assert_eq!(retry.outcome, Outcome::NotExecuted);
            let close = client.close().wait().await.unwrap_err();
            assert_eq!(close.message, "Hive storage stop is unconfirmed.");
            assert_eq!(close.outcome, Outcome::Unknown);
            assert_eq!(client.pending_count(), 0);
            assert!(!root.0.join("pid").exists());
            assert!(!root.0.join("hive.sqlite").exists());
        });
}
