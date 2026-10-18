use super::Root;
use ivy_contracts::{IvyError, JsString, Outcome, hash::digest};
use ivy_hive::store::{AuthContext, Credential, OnlineBackup, Store};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    cell::Cell,
    fs,
    future::poll_fn,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    rc::Rc,
    task::Poll,
};
use tokio::runtime::Builder;

fn original(cases: Vec<Value>, verify: Vec<PathBuf>) -> Value {
    let mut command =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()));
    command.arg("--no-warnings").arg(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tests/fixtures/hive-online-backup-oracle.mjs"),
    );
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            serde_json::to_string(&json!({"cases":cases,"verify":verify}))
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn seed(path: &Path, count: usize) {
    let mut store = Store::open(path).unwrap();
    store
        .configure_credentials(&[Credential {
            principal_id: "backup-user".into(),
            digest: digest("backup-token").into(),
        }])
        .unwrap();
    store.set_metadata("probe", "before").unwrap();
    store.connection().execute_batch("CREATE TABLE fixture_payload(id INTEGER PRIMARY KEY,bytes BLOB NOT NULL); BEGIN IMMEDIATE").unwrap();
    for i in 0..count {
        store
            .connection()
            .execute(
                "INSERT INTO fixture_payload VALUES(?,?)",
                rusqlite::params![i as i64, vec![(i % 251) as u8; 4096]],
            )
            .unwrap();
    }
    store.connection().execute_batch("COMMIT").unwrap();
}
fn sentinel(path: &Path) {
    Connection::open(path)
        .unwrap()
        .execute_batch("CREATE TABLE sentinel(value TEXT);INSERT INTO sentinel VALUES('retained')")
        .unwrap();
}
fn destination(path: &Path) -> JsString {
    path.to_str().unwrap().into()
}
fn outcome(result: Result<ivy_hive::store::BackupSnapshot, IvyError>, progress: usize) -> Value {
    match result {
        Ok(value) => json!({"value":super::wire(value.to_json()),"error":null,"progress":progress}),
        Err(error) => {
            json!({"value":null,"error":super::wire(error.to_wire()),"progress":progress})
        }
    }
}
fn compare(native: &Value, original: &Value) {
    assert_eq!(native["error"], original["error"]);
    assert_eq!(native["progress"], original["progress"]);
    if native["error"].is_null() {
        assert_eq!(native["value"]["pages"], original["value"]["pages"]);
    }
}
fn pending(task: &mut OnlineBackup) -> impl Future<Output = ()> + '_ {
    async {
        assert!(
            poll_fn(|cx| Poll::Ready(task.as_mut().poll(cx)))
                .await
                .is_pending(),
            "Expected a yielding incomplete copy"
        );
    }
}

#[test]
fn online_backup_copy_hash_and_progress_match_original_across_page_boundaries() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut cases = vec![];
            let mut native = vec![];
            let mut paths = vec![];
            for (index, count) in [0, 1, 16, 128, 256].into_iter().enumerate() {
                let base = root.0.join(format!("base-{index}.sqlite"));
                seed(&base, count);
                let source = root.0.join(format!("rust-{index}.sqlite"));
                fs::copy(&base, &source).unwrap();
                let node = root.0.join(format!("node-{index}.sqlite"));
                fs::copy(&base, &node).unwrap();
                let target = root.0.join(format!("snapshot-{index}-ä-😀.sqlite"));
                let node_target = root.0.join(format!("node-copy-{index}.sqlite"));
                if index % 2 == 1 {
                    sentinel(&target);
                    sentinel(&node_target);
                }
                let store = Store::open(&source).unwrap();
                let calls = Rc::new(Cell::new(0));
                let observing = calls.clone();
                let result = store
                    .backup_to(destination(&target), move || {
                        observing.set(observing.get() + 1);
                        Ok(())
                    })
                    .await;
                native.push(outcome(result, calls.get()));
                paths.push(target);
                cases.push(json!({"source":node,"destination":node_target}));
            }
            let expected = original(cases, paths.clone());
            for (index, result) in native.iter().enumerate() {
                compare(result, &expected["results"][index]);
                let actual = &expected["verified"][index];
                assert_eq!(result["value"]["bytesHash"], actual["digest"]);
                assert_eq!(result["value"]["pages"], actual["pages"]);
                assert_eq!(actual["check"], "ok");
                assert_eq!(
                    actual["payload"],
                    expected["results"][index]["state"]["payload"]
                );
                assert_eq!(
                    actual["credentials"],
                    expected["results"][index]["state"]["credentials"]
                );
                assert!(actual.get("sentinel").is_none());
            }
        });
}

#[test]
fn online_backup_interleaves_mutable_store_credentials_and_transactions_on_the_same_connection() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let base = root.0.join("base.sqlite");
            seed(&base, 256);
            let source = root.0.join("rust.sqlite");
            fs::copy(&base, &source).unwrap();
            let node = root.0.join("node.sqlite");
            fs::copy(&base, &node).unwrap();
            let target = root.0.join("snapshot.sqlite");
            let node_target = root.0.join("node-copy.sqlite");
            let mut store = Store::open(&source).unwrap();
            let calls = Rc::new(Cell::new(0));
            let observing = calls.clone();
            let mut task = store.backup_to(destination(&target), move || {
                observing.set(observing.get() + 1);
                Ok(())
            });
            pending(&mut task).await;
            assert_eq!(calls.get(), 1);
            store.configure_credentials(&[]).unwrap();
            assert_eq!(
                store
                    .authenticate(&AuthContext {
                        credential_digest: digest("backup-token").into(),
                        session_digest: None
                    })
                    .unwrap_err()
                    .code,
                "unauthenticated"
            );
            store
                .transaction(|s| {
                    s.set_metadata("probe", "after")?;
                    s.invalidate("test/backup".into());
                    Ok(())
                })
                .unwrap();
            let result = outcome(task.await, calls.get());
            let expected = original(
                vec![json!({"source":node,"destination":node_target,"mutate":true})],
                vec![target],
            );
            compare(&result, &expected["results"][0]);
            let actual = &expected["verified"][0];
            assert_eq!(result["value"]["bytesHash"], actual["digest"]);
            assert_eq!(actual["probe"], "after");
            assert_eq!(actual["credentials"][0]["revoked"], 1);
            assert_eq!(
                actual["payload"],
                expected["results"][0]["state"]["payload"]
            );
        });
}

#[test]
fn online_backup_transient_source_lock_yields_without_repeating_progress() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let base = root.0.join("base.sqlite");
            seed(&base, 256);
            let source = root.0.join("rust.sqlite");
            fs::copy(&base, &source).unwrap();
            let node = root.0.join("node.sqlite");
            fs::copy(&base, &node).unwrap();
            let target = root.0.join("snapshot.sqlite");
            let node_target = root.0.join("node-copy.sqlite");
            let store = Store::open(&source).unwrap();
            let calls = Rc::new(Cell::new(0));
            let observing = calls.clone();
            let mut task = store.backup_to(destination(&target), move || {
                observing.set(observing.get() + 1);
                Ok(())
            });
            pending(&mut task).await;
            assert_eq!(calls.get(), 1);
            store.connection().execute_batch("BEGIN IMMEDIATE").unwrap();
            for _ in 0..16 {
                pending(&mut task).await;
            }
            assert_eq!(calls.get(), 1);
            store.connection().execute_batch("ROLLBACK").unwrap();
            let result = outcome(task.await, calls.get());
            let expected = original(
                vec![json!({"source":node,"destination":node_target,"transientLock":true})],
                vec![target],
            );
            compare(&result, &expected["results"][0]);
            assert_eq!(
                result["value"]["bytesHash"],
                expected["verified"][0]["digest"]
            );
        });
}

#[test]
fn online_backup_callback_failures_and_dropped_copy_restore_destination_and_release_its_lock() {
    let root = Root::new();
    Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let base=root.0.join("base.sqlite");seed(&base,256);
        let mut cases=vec![];let mut native=vec![];let mut paths=vec![];
        for (index,phase) in ["copy","hash"].into_iter().enumerate() {
            let source=root.0.join(format!("rust-{index}.sqlite"));fs::copy(&base,&source).unwrap();
            let node=root.0.join(format!("node-{index}.sqlite"));fs::copy(&base,&node).unwrap();
            let target=root.0.join(format!("snapshot-{index}.sqlite"));let node_target=root.0.join(format!("node-copy-{index}.sqlite"));sentinel(&target);sentinel(&node_target);
            let store=Store::open(&source).unwrap();
            let pages=store.connection().query_row("PRAGMA page_count",[],|r|r.get::<_,i32>(0)).unwrap();
            let throw_at=if phase=="copy" {1} else {((pages-1)/100+1) as usize};
            let calls=Rc::new(Cell::new(0));let observing=calls.clone();
            let result=store.backup_to(destination(&target),move||{
                observing.set(observing.get()+1);
                if observing.get()==throw_at {Err(IvyError::new("fixture_backup_abort","Backup callback requested failure.").with_outcome(Outcome::Completed).with_details(ivy_contracts::IvyJson::object([("phase",phase.into())])))} else {Ok(())}
            }).await;
            native.push(outcome(result,calls.get()));paths.push(target);
            cases.push(json!({"source":node,"destination":node_target,"throwAt":throw_at,"phase":phase}));
        }
        let expected=original(cases,paths.clone());
        for (index,result) in native.iter().enumerate() {
            compare(result,&expected["results"][index]);
            assert_eq!(result["error"]["data"]["outcome"],"completed");
            let actual=&expected["verified"][index];let node=&expected["results"][index]["state"];
            assert_eq!(actual.get("sentinel"),node.get("sentinel"));
            assert_eq!(actual.get("payload"),node.get("payload"));
            let moved=paths[index].with_extension("released");fs::rename(&paths[index],&moved).unwrap();
        }
        let store=Store::open(&base).unwrap();let path=root.0.join("cancelled.sqlite");sentinel(&path);
        let mut task=store.backup_to(destination(&path),||Ok(()));pending(&mut task).await;drop(task);
        let db=Connection::open(&path).unwrap();assert_eq!(db.query_row("SELECT value FROM sentinel",[],|r|r.get::<_,String>(0)).unwrap(),"retained");drop(db);
        fs::rename(path,root.0.join("cancelled-released.sqlite")).unwrap();
    });
}

#[test]
fn online_backup_destination_errors_match_original_without_stricter_path_or_cleanup_policy() {
    let root = Root::new();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let base = root.0.join("base.sqlite");
            seed(&base, 128);
            let mut cases = vec![];
            let mut native = vec![];
            let mut paths = vec![];
            for index in 0..11 {
                let source = root.0.join(format!("rust-{index}.sqlite"));
                fs::copy(&base, &source).unwrap();
                let node = root.0.join(format!("node-{index}.sqlite"));
                fs::copy(&base, &node).unwrap();
                let target = root.0.join(format!("snapshot-{index}.sqlite"));
                let node_target = root.0.join(format!("node-copy-{index}.sqlite"));
                let (mut dest, mut node_dest) = (
                    destination(&target),
                    node_target.to_str().unwrap().to_string(),
                );
                let mut item = json!({"source":node});
                match index {
                    0 => {
                        fs::create_dir(&target).unwrap();
                        fs::create_dir(&node_target).unwrap();
                    }
                    1 => {
                        dest = destination(&target.join("missing/file.sqlite"));
                        node_dest = node_target
                            .join("missing/file.sqlite")
                            .to_str()
                            .unwrap()
                            .into();
                    }
                    2 => {
                        dest = JsString(vec![0]);
                        node_dest = "\0".into();
                    }
                    3 => {
                        dest = "".into();
                        node_dest = "".into();
                    }
                    4 => {
                        dest = ":memory:".into();
                        node_dest = ":memory:".into();
                    }
                    5 => {
                        fs::write(&target, b"not a database").unwrap();
                        fs::write(&node_target, b"not a database").unwrap();
                    }
                    6 => {
                        dest = destination(&source);
                        node_dest = node.to_str().unwrap().into();
                    }
                    7 => {
                        item["sourceLocked"] = true.into();
                    }
                    8 => {
                        sentinel(&target);
                        sentinel(&node_target);
                        item["targetLocked"] = true.into();
                    }
                    9 => {
                        let mut units = destination(&target).0;
                        units.extend([0xd800, 0xdc00, 0xd800]);
                        dest = JsString(units);
                        let mut node_units = destination(&node_target).0;
                        node_units.extend([0xd800, 0xdc00, 0xd800]);
                        item["units"] = json!(node_units);
                    }
                    10 => {
                        let uri = |path: &Path| {
                            let prefix = if cfg!(windows) { "file:///" } else { "file://" };
                            format!("{prefix}{}", path.to_str().unwrap().replace('\\', "/"))
                        };
                        dest = uri(&target).into();
                        node_dest = uri(&node_target);
                        item["inspect"] = json!(node_target);
                    }
                    _ => unreachable!(),
                }
                item["destination"] = node_dest.into();
                let store = Store::open(&source).unwrap();
                if index == 7 {
                    store.connection().execute_batch("BEGIN IMMEDIATE").unwrap();
                }
                let locker = if index == 8 {
                    let d = Connection::open(&target).unwrap();
                    d.execute_batch("BEGIN IMMEDIATE").unwrap();
                    Some(d)
                } else {
                    None
                };
                let calls = Rc::new(Cell::new(0));
                let observing = calls.clone();
                let result = store
                    .backup_to(dest.clone(), move || {
                        observing.set(observing.get() + 1);
                        Ok(())
                    })
                    .await;
                if index == 7 {
                    store.connection().execute_batch("ROLLBACK").unwrap();
                }
                if let Some(d) = locker {
                    d.execute_batch("ROLLBACK").unwrap();
                }
                drop(store);
                native.push(outcome(result, calls.get()));
                paths.push(if index == 10 {
                    target
                } else {
                    PathBuf::from(String::from_utf16_lossy(&dest.0))
                });
                cases.push(item);
            }
            let expected = original(cases, paths);
            for (index, result) in native.iter().enumerate() {
                compare(result, &expected["results"][index]);
                let actual = &expected["verified"][index];
                let node = &expected["results"][index]["state"];
                for key in [
                    "exists",
                    "directory",
                    "invalid",
                    "sentinel",
                    "payload",
                    "probe",
                ] {
                    assert_eq!(actual.get(key), node.get(key), "Case {index}, {key}");
                }
                if index == 9 {
                    assert!(result["error"].is_null());
                    assert_eq!(result["value"]["bytesHash"], actual["digest"]);
                } else {
                    assert_eq!(result["error"]["data"]["code"], "internal_error");
                    assert_eq!(result["error"]["data"]["outcome"], "unknown");
                }
            }
        });
}
