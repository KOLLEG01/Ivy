use super::*;

#[test]
fn initialized_kernel_backup_interleaves_credentials_and_defers_close_until_copy_finishes() {
    let mut cases = vec![];
    for completion in ["reply", "close", "timeout"] {
        let root = Root::new();
        let profile = original_profile(&root);
        let store = ivy_hive::store::Store::open(&root.0.join("hive.sqlite")).unwrap();
        store.connection().execute_batch("CREATE TABLE fixture_payload(id INTEGER PRIMARY KEY, bytes BLOB); WITH RECURSIVE n(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<256) INSERT INTO fixture_payload SELECT value,zeroblob(4096) FROM n;").unwrap();
        let source_pages: i64 = store
            .connection()
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap();
        assert!(source_pages > 100, "Copy must yield after its first batch");
        drop(store);
        let mut launch = kernel_launch(&root, &profile);
        launch.args = vec!["--kernel-gated-backup".into()];
        Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let events = Arc::new(Mutex::new(vec![]));
            let sink = events.clone();
            let client = StorageClient::new(launch, Arc::new(move |event| sink.lock().unwrap().push(event)));
            client.start().wait().await.unwrap();
            let ready = client.lifecycle();
            assert!(matches!(ready, Lifecycle::Ready(_)));
            let pid = fs::read_to_string(root.0.join("kernel-pid")).unwrap();
            assert!(database_locked(&root));
            let destination = root.0.join("copy.sqlite");
            let backup_payload = IvyJson::object([
                ("action", "backup".into()),
                ("destination", destination.to_str().unwrap().into()),
            ]);
            let mut backup = client.request(backup_payload.clone(), RequestOptions {
                timeout_ms: 45_000, ..Default::default()
            });
            wait_file(root.0.join("backup-paused")).await;
            assert!(matches!(backup.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
            let status = request(&client, IvyJson::object([
                ("action", "execute".into()),
                ("context", IvyJson::object([
                    ("credentialDigest", format!("sha256:{}", "a".repeat(64)).into()),
                    ("principalId", "alpha".into()), ("transport", "http".into()),
                ])),
                ("request", IvyJson::object([
                    ("jsonrpc", "2.0".into()), ("id", "status".into()),
                    ("method", "system.status".into()),
                    ("params", IvyJson::object(std::iter::empty::<(&str, IvyJson)>())),
                ])),
            ])).await.unwrap();
            assert_eq!(status.get("kind"), Some(&IvyJson::from("result")));
            assert_eq!(client.request(payload("health"), RequestOptions {
                timeout_ms: 1000, terminate_on_timeout: false, ..Default::default()
            }).await.unwrap().unwrap(), IvyJson::object([("ready", true.into())]));
            let digest = format!("sha256:{}", "b".repeat(64));
            let configured = request(&client, IvyJson::object([
                ("action", "credentials.configure".into()),
                ("credentials", IvyJson::Array(vec![IvyJson::object([
                    ("principalId", "beta".into()), ("digest", digest.clone().into()),
                ])])),
            ])).await.unwrap();
            assert_eq!(configured.get("configured"), Some(&IvyJson::Bool(true)));
            let authenticate = |digest: String| IvyJson::object([
                ("action", "authenticate".into()),
                ("context", IvyJson::object([("credentialDigest", digest.into())])),
            ]);
            assert_eq!(request(&client, authenticate(format!("sha256:{}", "a".repeat(64)))).await.unwrap_err().code, "unauthenticated");
            assert_eq!(request(&client, authenticate(digest)).await.unwrap().get("principalId"), Some(&IvyJson::from("beta")));
            let second = request(&client, backup_payload).await.unwrap_err();
            assert_eq!(second.code, "limit_exceeded");
            assert_eq!(second.message, "A storage backup is already active.");

            // Only the client's Tokio clock advances. Resume before awaiting
            // real child I/O or OS retirement; no wall-clock sleep is needed.
            assert_eq!(RequestOptions::default().timeout_ms, 30_000);
            let baseline = tokio::time::Instant::now();
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(35)).await;
            for _ in 0..32 { tokio::task::yield_now().await; }
            tokio::time::resume();
            assert!(baseline.elapsed() >= Duration::from_secs(35));
            assert!(matches!(backup.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
            assert_eq!(client.lifecycle(), ready);
            assert_eq!(request(&client, payload("health")).await.unwrap().get("ready"), Some(&IvyJson::Bool(true)));
            assert_eq!(fs::read_to_string(root.0.join("kernel-pid")).unwrap(), pid);
            assert!(database_locked(&root));
            assert!(events.lock().unwrap().iter().all(|event| !matches!(event, Event::Failure(_))));

            if completion == "timeout" {
                // The first real copy batch is still held, so no later
                // Progress can rearm this backup's 45-second deadline.
                tokio::time::pause();
                tokio::time::advance(Duration::from_secs(15)).await;
                for _ in 0..32 { tokio::task::yield_now().await; }
                tokio::time::resume();
                let error = timeout(Duration::from_secs(5), backup).await.unwrap().unwrap().unwrap_err();
                assert_eq!(error.code, "deadline_exceeded");
                assert_eq!(error.outcome, Outcome::Unknown);
                stopped(&client).await;
                assert!(!root.0.join("release-backup").exists());
                assert!(!database_locked(&root));
                assert_eq!(client.pending_count(), 0);
                assert_eq!(client.queued_bytes(), 0);
                let failures = events.lock().unwrap().iter().filter(|event| matches!(event, Event::Failure(_))).count();
                assert_eq!(failures, 1);
                client.close().wait().await.unwrap();
                cases.push(serde_json::json!({
                    "case":"initialized-backup-without-progress", "clientClockAdvancedMs":50_000,
                    "errorCode":error.code,"outcome":format!("{:?}",error.outcome),
                    "failureEvents":failures,"copyReleased":false,"databaseLockedAfterStop":false,
                    "pendingAfterStop":0,"queuedBytesAfterStop":0,
                    "productionEntryExecuted":false,"originalReplay":false
                }));
                return;
            }

            let closing = if completion == "close" {
                let closing = client.close();
                assert!(timeout(Duration::from_millis(100), closing.wait()).await.is_err());
                assert!(matches!(backup.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
                Some(closing)
            } else { None };
            fs::write(root.0.join("release-backup"), b"resume actual copy").unwrap();
            let snapshot = timeout(Duration::from_secs(10), backup).await.unwrap().unwrap().unwrap();
            if let Some(closing) = closing {
                timeout(Duration::from_secs(5), closing.wait()).await.unwrap().unwrap();
            } else {
                assert_eq!(request(&client, payload("health")).await.unwrap(), IvyJson::object([("ready", true.into())]));
                assert_eq!(client.lifecycle(), ready);
                assert_eq!(fs::read_to_string(root.0.join("kernel-pid")).unwrap(), pid);
                assert!(database_locked(&root));
                assert!(events.lock().unwrap().iter().all(|event| !matches!(event, Event::Failure(_))));
                assert_eq!(client.pending_count(), 0);
                assert_eq!(client.queued_bytes(), 0);
                client.close().wait().await.unwrap();
            }
            stopped(&client).await;
            assert!(!database_locked(&root));
            assert_eq!(client.pending_count(), 0);
            assert_eq!(client.queued_bytes(), 0);
            let bytes_hash = ivy_contracts::hash::digest(fs::read(&destination).unwrap());
            assert_eq!(snapshot.get("bytesHash"), Some(&bytes_hash.clone().into()));
            let copy = rusqlite::Connection::open(destination).unwrap();
            let pages: i64 = copy.query_row("PRAGMA page_count", [], |row| row.get(0)).unwrap();
            assert!(pages > 0);
            assert_eq!(snapshot.get("pages").and_then(IvyJson::as_f64), Some(pages as f64));
            assert_eq!(copy.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
            assert_eq!(copy.query_row("SELECT COUNT(*) FROM fixture_payload", [], |row| row.get::<_, i64>(0)).unwrap(), 256);
            let revoked = |principal: &str| copy.query_row("SELECT revoked FROM credentials WHERE principal_id=?", [principal], |row| row.get::<_, i64>(0)).unwrap();
            assert_eq!(revoked("alpha"), 1);
            assert_eq!(revoked("beta"), 0);
            cases.push(serde_json::json!({
                "case":completion,"sourcePagesBeforeStartup":source_pages,
                "snapshotPages":pages,"bytesHash":bytes_hash,"payloadRows":256,
                "credentialsAfterPausedCopy":{"alphaRevoked":1,"betaRevoked":0},
                "ordinaryStatusWhilePaused":true,"ordinaryHealthWhilePaused":true,
                "normalRequestTimeoutMs":30_000,"backupTimeoutMs":45_000,"clientClockAdvancedMs":35_000,
                "sameGenerationAndPidWhilePaused":true,"failureEventsWhilePaused":0,
                "readyAfterCopy":completion == "reply","sameGenerationAndPidAfterCopy":completion == "reply",
                "secondBackupCode":second.code,"secondBackupOutcome":format!("{:?}",second.outcome),
                "closePendingForMs":if completion == "close" {100} else {0},"backupPendingBeforeRelease":true,
                "databaseLockedBeforeCopy":true,"databaseLockedAfterDefiniteStop":false,
                "pendingAfterStop":client.pending_count(),"queuedBytesAfterStop":client.queued_bytes(),
                "testGate":"after first real yielding batch; real initialization/dispatch/backup",
                "productionEntryExecuted":false,"originalReplay":false,"closeReplyClaimed":false
            }));
        });
    }
    save_kernel_process_cases(serde_json::json!(cases));
}

#[test]
fn real_worker_backup_interleaves_committed_writes_rejects_second_backup_and_close_waits() {
    for close_during_copy in [false, true] {
        let root = Root::new();
        Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let (client, events) = client(&root);
                client.start().wait().await.unwrap();
                let ready = client.lifecycle();
                let destination = root.0.join("copy.sqlite");
                let backup_payload = IvyJson::object([
                    ("action", "backup".into()),
                    ("destination", destination.to_str().unwrap().into()),
                    ("delayMs", IvyJson::Number(3000.0)),
                ]);
                let started = tokio::time::Instant::now();
                let backup = client.request(
                    backup_payload.clone(),
                    RequestOptions {
                        // Original progress throttle is 1000ms. The actual task is
                        // held longer than this deadline and emits fixture heartbeats.
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
                let close = close_during_copy.then(|| client.close());
                let pages = backup.await.unwrap().unwrap();
                assert!(started.elapsed() >= Duration::from_millis(3000));
                assert!(pages.get("pages").unwrap().as_f64().unwrap() > 0.0);
                if let Some(close) = close {
                    close.wait().await.unwrap();
                } else {
                    assert_eq!(client.lifecycle(), ready);
                    assert_eq!(
                        request(&client, payload("health")).await.unwrap(),
                        IvyJson::object([("ready", true.into())])
                    );
                    assert!(
                        events
                            .lock()
                            .unwrap()
                            .iter()
                            .all(|event| !matches!(event, Event::Failure(_)))
                    );
                    client.close().wait().await.unwrap();
                }
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
}
