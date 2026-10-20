//! One real owned child, original-library protocol calls and committed Rust DB.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ivy_chat_bridge::whatsapp::{
    auth::{AuthMutation, AuthStore},
    auth_node::{AuthAccount, AuthLaunch, AuthMethod, AuthNode, AuthReply},
};
use ivy_contracts::{
    IvyJson as Json, JsString, Outcome,
    json::{parse, stringify},
};
use ivy_host_runtime::{node_runtime::NodeRuntime, portable_native_files::NativeHelperLayout};
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
    time::Duration,
};

fn artifact() -> PathBuf {
    ivy_host_runtime::paths::realpath(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .unwrap()
}
fn read(path: impl AsRef<Path>) -> Json {
    parse(&fs::read_to_string(path).unwrap(), usize::MAX).unwrap()
}
fn encode(value: &Json) -> String {
    stringify(value, usize::MAX).unwrap()
}
fn buffer(bytes: &[u8]) -> Json {
    Json::object([
        ("type", "Buffer".into()),
        ("data", STANDARD.encode(bytes).into()),
    ])
}
fn raw(db: &Path, category: &str, id: &str) -> Option<String> {
    AuthStore::open(db).unwrap().get(category, id).unwrap()
}
fn session(db: &Path) -> String {
    raw(db, "session", "15555550101.1").unwrap()
}
fn current_root(encoded: &str) -> Json {
    let row = parse(encoded, usize::MAX).unwrap();
    row.required("_sessions")
        .unwrap()
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, v)| {
            v.required("indexInfo")
                .unwrap()
                .get("closed")
                .and_then(Json::as_f64)
                == Some(-1.0)
        })
        .unwrap()
        .1
        .required("currentRatchet")
        .unwrap()
        .required("rootKey")
        .unwrap()
        .clone()
}
fn fixture(name: &str) -> PathBuf {
    let parent = std::env::var_os("IVY_CHAT_AUTH_NODE_SCRATCH")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = parent.join(format!("{name}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("whatsapp-owner")).unwrap();
    // A scoped qualification can reuse its retained original corpus. Ordinary
    // repository runs generate it once through the existing original fixture.
    static CORPUS: OnceLock<PathBuf> = OnceLock::new();
    let corpus = CORPUS.get_or_init(|| {
        std::env::var_os("IVY_CHAT_AUTH_NODE_CORPUS")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let path = std::env::temp_dir()
                    .join(format!("ivy-auth-node-corpus-{}", uuid::Uuid::new_v4()));
                fs::create_dir(&path).unwrap();
                super::auth_compat::phase("seed", &path);
                path
            })
    });
    for (from, to) in [
        ("original.sqlite", "auth.sqlite"),
        ("alice.sqlite", "alice.sqlite"),
        ("wire.json", "wire.json"),
    ] {
        fs::copy(corpus.join(from), root.join(to)).unwrap();
    }
    root
}
fn account(root: &Path) -> AuthAccount {
    AuthAccount::acquire(
        &root.join("auth.sqlite"),
        &root.join("whatsapp-owner/executor-lock.sqlite"),
    )
    .unwrap()
}
fn launch(root: &Path, fault: Option<&str>) -> AuthLaunch {
    let mut environment = BTreeMap::new();
    let mut node_options = std::env::var("NODE_OPTIONS").unwrap_or_default();
    let (node, loader, fault_loader) = node_fixture();
    node_options.push_str(&format!(" --import={loader} "));
    if let Some(fault) = fault {
        node_options.push_str(&format!(" --import={fault_loader} "));
        environment.insert("IVY_CHAT_AUTH_FAULT".into(), fault.into());
        environment.insert(
            "IVY_CHAT_AUTH_GATE".into(),
            root.join("gate").into_os_string(),
        );
    }
    environment.insert("NODE_OPTIONS".into(), node_options.into());
    // The fixture stages just this admitted worker and the existing Host helper;
    // it never writes a helper into the source tree or implements a process owner.
    let staged = root.join("artifact");
    let scripts = staged.join("services/chat-bridge/rust/node");
    fs::create_dir_all(&scripts).unwrap();
    for file in ["auth-worker.mjs", "auth-adapter.mjs", "auth-frames.mjs"] {
        fs::copy(
            artifact().join("services/chat-bridge/rust/node").join(file),
            scripts.join(file),
        )
        .unwrap();
    }
    #[cfg(windows)]
    {
        let helper = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("ivy-job.exe");
        fs::create_dir_all(staged.join("dist/native")).unwrap();
        fs::copy(helper, staged.join("dist/native/ivy-job.exe"))
            .expect("The shared runner must build the Host ivy-job prerequisite");
    }
    AuthLaunch {
        node: node.clone(),
        artifact_root: staged,
        helper_layout: NativeHelperLayout::Legacy,
        environment,
        deadline: Duration::from_secs(8),
    }
}
fn node_fixture() -> &'static (NodeRuntime, String, String) {
    static NODE: OnceLock<(NodeRuntime, String, String)> = OnceLock::new();
    NODE.get_or_init(|| {
        let executable=std::env::var_os("IVY_CHAT_NODE_EXECUTABLE").unwrap_or_else(||"node".into());
        let output=Command::new(executable).args(["--input-type=commonjs","-e",r#"const{pathToFileURL}=require('node:url'),{join}=require('node:path');const root=process.argv[1];process.stdout.write(JSON.stringify({executable:process.execPath,version:process.versions.node,loader:pathToFileURL(join(root,'tests/rust/fixtures/chat-auth-node-loader.mjs')).href,fault:pathToFileURL(join(root,'tests/rust/fixtures/chat-auth-node-fault.mjs')).href}));"#]).arg(artifact()).output().unwrap();
        assert!(output.status.success(),"Node fixture metadata unavailable");
        let value=parse(std::str::from_utf8(&output.stdout).unwrap(),usize::MAX).unwrap();
        (NodeRuntime {executable:PathBuf::from(value.required("executable").unwrap().as_str().unwrap()),version:value.required("version").unwrap().as_str().unwrap()},value.required("loader").unwrap().as_str().unwrap(),value.required("fault").unwrap().as_str().unwrap())
    })
}
async fn call(node: &mut AuthNode, method: AuthMethod, args: Json) -> Json {
    match node
        .call(
            JsString::from(format!("synthetic-operation:{}", uuid::Uuid::new_v4())),
            method,
            encode(&args),
        )
        .await
        .unwrap()
    {
        AuthReply::Value(Some(result)) => parse(&result, usize::MAX).unwrap(),
        AuthReply::Value(None) => Json::Null,
        AuthReply::LibraryError(name) => panic!("Unexpected categorical library error: {name}"),
    }
}
fn original(mode: &str, root: &Path) -> Json {
    let result = Command::new(&node_fixture().0.executable)
        .current_dir(artifact())
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/rust/fixtures/chat-auth-node.mjs",
            mode,
        ])
        .arg(root)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "Original fixture failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    parse(std::str::from_utf8(&result.stdout).unwrap(), usize::MAX).unwrap()
}
fn lock_held(root: &Path) -> bool {
    original("lock", root)
        .get("acquired")
        .and_then(Json::as_bool)
        == Some(false)
}
fn wire_message(wire: &Json, kind: &str, index: usize) -> Json {
    let mut value = wire.required(kind).unwrap().as_array().unwrap()[index].clone();
    let ciphertext = value.required("ciphertext").unwrap().as_str().unwrap();
    value.insert("ciphertext", buffer(&STANDARD.decode(ciphertext).unwrap()));
    value.insert("jid", "15555550101:1@s.whatsapp.net".into());
    value
}
fn stats(node: &AuthNode) -> Json {
    let s = node.statistics();
    Json::object([
        ("sentFrames", Json::Number(s.sent_frames as f64)),
        ("receivedFrames", Json::Number(s.received_frames as f64)),
        (
            "maximumFrameBytesIncludingHeader",
            Json::Number((s.maximum_frame_bytes + 4) as f64),
        ),
        ("committedBatches", Json::Number(s.committed_batches as f64)),
    ])
}

#[test]
fn isolated_auth_boundary_preserves_current_state_and_fences_uncertainty() {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let root=fixture("current");let db=root.join("auth.sqlite");let wire=read(root.join("wire.json"));
        let mut before=AuthStore::open(&db).unwrap();let original_rows=before.snapshot().unwrap();before.close().unwrap();
        assert_eq!(original_rows.len(),13);
        assert_eq!(original_rows.iter().map(|r|&r.category).collect::<std::collections::BTreeSet<_>>().len(),12);
        let mut node=AuthNode::launch(account(&root),17,"synthetic-init-current".into(),launch(&root,None)).await.unwrap();
        assert!(lock_held(&root),"Original owner must be excluded while child lives");
        assert!(AuthAccount::acquire(&db,&root.join("whatsapp-owner/executor-lock.sqlite")).is_err(),"Second generation/account child cannot be admitted");
        let decrypted=call(&mut node,AuthMethod::DecryptMessage,wire_message(&wire,"current",0)).await;
        assert!(decrypted==buffer(b"current-0"));
        let reply=call(&mut node,AuthMethod::EncryptMessage,Json::object([("jid","15555550101:1@s.whatsapp.net".into()),("data",buffer(b"boundary-reply"))])).await;
        fs::write(root.join("reply.json"),encode(&reply)).unwrap();
        let peer=original("peer-reply",&root);assert_eq!(peer.get("peerDecrypted").and_then(Json::as_bool),Some(true));
        let mut followup=parse(&peer.required("followup").unwrap().as_str().unwrap(),usize::MAX).unwrap();followup.insert("jid","15555550101:1@s.whatsapp.net".into());
        assert!(call(&mut node,AuthMethod::DecryptMessage,followup).await==buffer(b"boundary-followup"));
        let keys=call(&mut node,AuthMethod::GetKeys,Json::object([("type","app-state-sync-key".into()),("ids",Json::Array(vec!["synthetic-sync".into()]))])).await;
        assert_eq!(keys.required("synthetic-sync").unwrap().required("timestamp").unwrap().as_str().unwrap(),"1730000000000");
        let mut version=call(&mut node,AuthMethod::GetKeys,Json::object([("type","app-state-sync-version".into()),("ids",Json::Array(vec!["regular".into()]))])).await;
        let mut regular=version.required("regular").unwrap().clone();regular.insert("version",Json::Number(8.0));version.insert("regular",regular);
        call(&mut node,AuthMethod::SetKeys,Json::object([("app-state-sync-version",version)])).await;
        let mut creds=call(&mut node,AuthMethod::Credentials,Json::Null).await;creds.insert("accountSyncCounter",Json::Number(5.0));
        assert!(creds.required("additionalData").unwrap().required("unknown").unwrap().as_js_string().unwrap().0==[0xdfff]);
        call(&mut node,AuthMethod::SaveCredentials,creds).await;
        // Multi-frame auth row and response, escaped values and original SQL
        // admission of lone-surrogate category/ids, without a total row cap.
        let long=JsString([vec![0x61;300_000],vec![0xd800,0,0x20ac]].concat());
        let key=JsString(vec![0xdfff]);
        let data=Json::Object(vec![("future-category".into(),Json::Object(vec![("large".into(),Json::String(long.clone())),(key.clone(),"surrogate-id".into()),("remove".into(),Json::Null)]))]);
        call(&mut node,AuthMethod::SetKeys,data).await;
        let back=call(&mut node,AuthMethod::GetKeys,Json::object([("type","future-category".into()),("ids",Json::Array(vec!["large".into(),Json::String(key.clone())]))])).await;
        assert!(back.required("large").unwrap().as_js_string()==Some(&long));
        assert!(back.get_key(&key)==Some(&Json::from("surrogate-id")));
        let current_rows={let mut store=AuthStore::open(&db).unwrap();let rows=store.snapshot().unwrap();store.close().unwrap();rows};
        for row in original_rows.iter().filter(|r|!["creds","session","app-state-sync-version"].contains(&r.category.as_str())) {
            assert!(current_rows.iter().any(|r|r.category==row.category&&r.id==row.id&&r.value==row.value),"Untouched raw auth row changed");
        }
        let normal_stats=stats(&node);assert!(node.statistics().sent_frames>10&&node.statistics().maximum_frame_bytes+4<=65536);
        node.shutdown().await.unwrap();assert!(node.ownership_released());assert!(!lock_held(&root),"Confirmed stop releases original lock");
        let reopen=original("reopen",&root);assert_eq!(reopen.get("sameEvolvedDatabase").and_then(Json::as_bool),Some(true));

        // Same retained original sender-key/archived corpus; the baseline
        // defects remain completed negative findings through the new boundary.
        let negative_root=fixture("negative");let negative_db=negative_root.join("auth.sqlite");let wire=read(negative_root.join("wire.json"));
        let mut negative=AuthNode::launch(account(&negative_root),18,"synthetic-init-negative".into(),launch(&negative_root,None)).await.unwrap();
        let before_group=raw(&negative_db,"sender-key","synthetic-auth@g.us::15555550101::1");assert!(before_group.is_some());
        let result=negative.call("synthetic-group-control".into(),AuthMethod::DecryptGroupMessage,encode(&Json::object([("group","synthetic-auth@g.us".into()),("authorJid","15555550101:1@s.whatsapp.net".into()),("msg",wire.required("group").unwrap().as_array().unwrap()[0].clone())]))).await.unwrap();
        assert!(matches!(result,AuthReply::LibraryError(ref name)if name=="TypeError"));
        assert!(raw(&negative_db,"sender-key","synthetic-auth@g.us::15555550101::1")==before_group);
        let before_root=current_root(&session(&negative_db));
        assert!(call(&mut negative,AuthMethod::DecryptMessage,wire_message(&wire,"archived",0)).await==buffer(b"archived-0"));
        assert!(current_root(&session(&negative_db))!=before_root);
        let reply=call(&mut negative,AuthMethod::EncryptMessage,Json::object([("jid","15555550101:1@s.whatsapp.net".into()),("data",buffer(b"archived-probe-reply"))])).await;
        fs::write(negative_root.join("reply.json"),encode(&reply)).unwrap();
        let archived=original("archived-peer",&negative_root);assert_eq!(archived.get("conditionMet").and_then(Json::as_bool),Some(false));
        negative.shutdown().await.unwrap();

        // A lost stop reply is an unresolved stop, even when Host subsequently
        // confirms tree retirement. It cannot release/restart account ownership.
        let stopped_root=fixture("lost-stop-reply");
        let mut stopped=AuthNode::launch(account(&stopped_root),21,"synthetic-init-stop-loss".into(),launch(&stopped_root,Some("lost-stop-reply"))).await.unwrap();
        assert!(stopped.shutdown().await.is_err());assert!(stopped.is_fenced());
        assert!(stopped.unknown_operation().is_some());assert!(!stopped.ownership_released());
        assert!(lock_held(&stopped_root));

        // Real commit acknowledgement held at the child's input. No caller
        // completion is possible while the Rust commit ACK is withheld.
        let gate_root=fixture("ack-gate");let gate_db=gate_root.join("auth.sqlite");
        let mut gated=AuthNode::launch(account(&gate_root),19,"synthetic-init-gate".into(),launch(&gate_root,Some("hold-commit-ack"))).await.unwrap();
        {
        let task=gated.call("synthetic-commit-gate".into(),AuthMethod::SetKeys,encode(&Json::object([("future-category",Json::object([("gate","committed-value".into())]))])));
        tokio::pin!(task);
        while !gate_root.join("gate.observed").exists() { tokio::select! { _ = &mut task => panic!("Completion preceded committed ACK"), _ = tokio::time::sleep(Duration::from_millis(10))=>{} } }
        assert!(raw(&gate_db,"future-category","gate").as_deref()==Some("\"committed-value\""));
        assert!(tokio::time::timeout(Duration::from_millis(60),&mut task).await.is_err());
        fs::write(gate_root.join("gate.release"),"continue").unwrap();task.await.unwrap();
        }
        gated.shutdown().await.unwrap();

        let mut faults=Vec::new();
        for fault in ["before-commit","commit-ack-loss","after-crypto","stale-generation","stale-request","partial","invalid-batch","atomic-rollback"] {
            let root=fixture(fault);let db=root.join("auth.sqlite");let before=session(&db);
            let mut node=AuthNode::launch(account(&root),20,"synthetic-init-fault".into(),launch(&root,if fault=="atomic-rollback"{None}else{Some(fault)})).await.unwrap();
            if fault=="atomic-rollback" { let conn=Connection::open(&db).unwrap();conn.execute_batch("CREATE TRIGGER reject_batch BEFORE INSERT ON auth WHEN NEW.id='reject' BEGIN SELECT RAISE(ABORT,'synthetic sensitive diagnostic'); END;").unwrap();conn.close().unwrap(); }
            let operation=JsString::from(format!("synthetic-{fault}-original"));
            let (method,args)=if fault=="after-crypto" { (AuthMethod::EncryptMessage,Json::object([("jid","15555550101:1@s.whatsapp.net".into()),("data",buffer(b"lost-crypto-result"))])) } else { (AuthMethod::SetKeys,Json::object([("future-category",Json::object([("probe","new-value".into()),("reject","reject-value".into())]))])) };
            let error=node.call(operation.clone(),method,encode(&args)).await.err().expect("Uncertain call must not return success");
            assert_eq!(error.outcome,Outcome::Unknown);assert!(node.is_fenced());assert!(node.unknown_operation()==Some(&operation));
            let committed=raw(&db,"future-category","probe").is_some();
            assert_eq!(committed,fault=="commit-ack-loss");
            assert_eq!(session(&db)!=before,fault=="after-crypto");
            let sent=node.statistics().sent_frames;
            assert!(node.call(operation.clone(),method,encode(&args)).await.is_err());
            assert_eq!(node.statistics().sent_frames,sent,"No replay or restarted callback");
            assert!(node.shutdown().await.is_err());assert!(!node.ownership_released());assert!(lock_held(&root));
            assert!(AuthAccount::acquire(&db,&root.join("whatsapp-owner/executor-lock.sqlite")).is_err(),"Fenced generation cannot restart");
            faults.push(Json::object([("fault",fault.into()),("outcome","unknown".into()),("originalOperationRetained",true.into()),("committed",committed.into()),("cryptoAdvanced",(fault=="after-crypto").into()),("replayRefused",true.into()),("ownershipRetained",true.into()),("statistics",stats(&node))]));
        }
        let capture=Json::object([("synthetic",true.into()),("originalRows",super::auth_compat::rows(&original_rows)),("currentEvolvedRows",super::auth_compat::rows(&current_rows)),("normalStatistics",normal_stats),("sameCurrentDbReopen",reopen),("legacyGroupControl",Json::object([("execution","completed".into()),("conditionMet",false.into()),("error","TypeError".into()),("encodedStateUnchanged",true.into())])),("legacyArchivedControl",archived),("commitBeforeCompletion",true.into()),("oneChildPerAccount",true.into()),("lostStopRetainsOwnership",true.into()),("faults",Json::Array(faults)),("liveSocket",false.into()),("productionBound",false.into())]);
        if let Some(path)=std::env::var_os("IVY_CHAT_AUTH_NODE_CAPTURE") {fs::write(path,encode(&capture)).unwrap();}
    });
}

#[test]
fn ordered_auth_batches_match_original_noops_and_rollback() {
    let root = fixture("ordered");
    let rust = root.join("rust.sqlite");
    let original_path = root.join("auth.sqlite");
    // Fresh copies have identical test-only triggers, not production tables.
    for db in [&rust, &original_path] {
        if db == &original_path {
            fs::remove_file(db).unwrap();
        }
        let store = AuthStore::open(db).unwrap();
        store.set("future-category", "base", Some("{}")).unwrap();
        store.close().unwrap();
        let c = Connection::open(db).unwrap();
        c.execute_batch("CREATE TABLE observed(kind TEXT); CREATE TRIGGER inserts BEFORE INSERT ON auth BEGIN INSERT INTO observed VALUES('insert'); END; CREATE TRIGGER updates BEFORE UPDATE ON auth BEGIN INSERT INTO observed VALUES('update'); END; CREATE TRIGGER deletes BEFORE DELETE ON auth BEGIN INSERT INTO observed VALUES('delete'); END;").unwrap();
        c.close().unwrap();
    }
    let entries = Json::Array(
        vec![
            ("base", Some("{}")),
            ("missing", None),
            ("ordered", Some("1")),
            ("ordered", Some("1")),
            ("ordered", None),
            ("ordered", Some("2")),
            ("base", None),
            ("base", None),
        ]
        .into_iter()
        .map(|(id, value)| {
            Json::Array(vec![
                "future-category".into(),
                id.into(),
                value.map(Json::from).unwrap_or(Json::Null),
            ])
        })
        .collect(),
    );
    fs::write(root.join("entries.json"), encode(&entries)).unwrap();
    let original = original("ordered-batch", &root);
    let mut store = AuthStore::open(&rust).unwrap();
    let values = entries
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            let p = v.as_array().unwrap();
            (
                p[1].as_str().unwrap(),
                if p[2].is_null() {
                    None
                } else {
                    Some(p[2].as_str().unwrap())
                },
            )
        })
        .collect::<Vec<_>>();
    store
        .set_batch(
            &values
                .iter()
                .map(|(id, value)| AuthMutation {
                    category: "future-category",
                    id,
                    value: value.as_deref(),
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
    assert!(
        super::auth_compat::rows(&store.snapshot().unwrap()) == *original.required("rows").unwrap()
    );
    store.close().unwrap();
    let c = Connection::open(&rust).unwrap();
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM observed", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        original.required("changes").unwrap().as_f64().unwrap() as i64
    );
    c.close().unwrap();
}
