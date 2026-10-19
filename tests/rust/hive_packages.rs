use super::*;
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ivy_contracts::IvyResult;
use ivy_hive::{
    package_catalog_object::PackageCatalogObject,
    packages::{
        PackageClock, PackageInstallation, PackageOptions, PackageRegistry, PackageRequest,
        PackageStorage, PackageUiInstallation, PackageUiInstaller, StagedPackageUpload,
    },
};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
        mpsc,
    },
    thread,
};
use tokio::{
    io::{AsyncRead, ReadBuf},
    sync::oneshot,
};

enum CommandMessage {
    Request(IvyJson, oneshot::Sender<IvyResult<IvyJson>>),
    Control(Value, mpsc::Sender<Value>),
}
struct Clock {
    now: AtomicI64,
    iso: HashMap<i64, JsString>,
}
impl PackageClock for Clock {
    fn now(&self) -> f64 {
        self.now.load(Ordering::SeqCst) as f64
    }
    fn iso_date(&self, milliseconds: f64) -> IvyResult<JsString> {
        Ok(self
            .iso
            .get(&(milliseconds as i64))
            .expect("Observed original ISO primitive")
            .clone())
    }
}
struct Actor {
    sender: mpsc::Sender<CommandMessage>,
    requests: Arc<Mutex<Vec<Value>>>,
    failure: Arc<Mutex<Option<Value>>>,
}
impl PackageStorage for Actor {
    fn request(&self, request: IvyJson) -> PackageRequest {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(CommandMessage::Request(request, sender))
            .unwrap();
        Box::pin(async move { receiver.await.unwrap() })
    }
}
impl Actor {
    fn start(path: PathBuf, clock: Arc<Clock>) -> (Arc<Self>, thread::JoinHandle<()>) {
        let (sender, receiver) = mpsc::channel();
        let requests = Arc::new(Mutex::new(vec![]));
        let failure = Arc::new(Mutex::new(None::<Value>));
        let actor = Arc::new(Self {
            sender,
            requests: requests.clone(),
            failure: failure.clone(),
        });
        let task = thread::spawn(move || {
            fn protected(_: &Store) -> IvyResult<HashSet<JsString>> {
                Ok(HashSet::new())
            }
            let open = || {
                let mut store = Store::open(&path).unwrap();
                store.connection().execute_batch("UPDATE metadata SET value='epoch' WHERE key='runtime_epoch'; INSERT OR IGNORE INTO principals VALUES('publisher'),('other');").unwrap();
                let owner = PackageCatalogObject::new(
                    &mut store,
                    protected as fn(&Store) -> IvyResult<HashSet<JsString>>,
                )
                .unwrap();
                (store, owner)
            };
            let (mut store, mut owner) = open();
            while let Ok(message) = receiver.recv() {
                match message {
                    CommandMessage::Request(request, reply) => {
                        requests.lock().unwrap().push(wire(request.clone()));
                        let action = request.get("action").unwrap().as_str().unwrap();
                        let forced = {
                            let mut next = failure.lock().unwrap();
                            if next.as_ref().is_some_and(|f| f["action"] == action) {
                                next.take()
                            } else {
                                None
                            }
                        };
                        let result = if let Some(f) = forced {
                            Err(IvyError::new(
                                f["code"].as_str().unwrap(),
                                "Fixture worker failure.",
                            ))
                        } else {
                            let now = clock.now() as u64;
                            match action.as_str() {
                                "package.catalog" => owner
                                    .list(
                                        &store,
                                        request.get("after").unwrap(),
                                        request.get("includeUis").unwrap().as_bool().unwrap(),
                                    )
                                    .map(|v| v.into_json()),
                                "package.seed" => owner
                                    .seed(
                                        &mut store,
                                        request.get("catalog").unwrap(),
                                        &"publisher".into(),
                                        now,
                                        &clock.iso_date(now as f64).unwrap(),
                                    )
                                    .map(|v| v.into_json()),
                                "package.prune" => owner
                                    .prune(
                                        &mut store,
                                        &"publisher".into(),
                                        now,
                                        &clock.iso_date(now as f64).unwrap(),
                                    )
                                    .map(IvyJson::Array),
                                "package.publish" => owner.publish(
                                    &mut store,
                                    request.get("input").unwrap(),
                                    request
                                        .get("publisherPrincipalId")
                                        .unwrap()
                                        .as_js_string()
                                        .unwrap(),
                                    now,
                                    &clock.iso_date(now as f64).unwrap(),
                                ),
                                _ => panic!("Unexpected worker action"),
                            }
                        };
                        let _ = reply.send(result);
                    }
                    CommandMessage::Control(step, reply) => {
                        match step["method"].as_str().unwrap() {
                            "sql" => {
                                store
                                    .connection()
                                    .execute_batch(step["sql"].as_str().unwrap())
                                    .unwrap();
                            }
                            "reopen" => {
                                drop(store);
                                (store, owner) = open();
                            }
                            "snapshot" => {}
                            "install" => {
                                store.connection().execute_batch("INSERT INTO metadata VALUES('fixture_ui_installations','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1;").unwrap();
                            }
                            _ => panic!("Unexpected actor control"),
                        }
                        reply.send(json!({"state":snapshot(&store,&json!({})),"catalogMutations":rows(&store,"SELECT mutation_id FROM mutations ORDER BY rowid"),"definitions":rows(&store,"SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type,name")})).unwrap();
                    }
                }
            }
        });
        (actor, task)
    }
    fn control(&self, step: Value) -> Value {
        let (sender, receiver) = mpsc::channel();
        self.sender
            .send(CommandMessage::Control(step, sender))
            .unwrap();
        receiver.recv().unwrap()
    }
}
fn node(input: Value) -> Value {
    let mut child = std::process::Command::new(
        std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()),
    )
    .arg(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tests/fixtures/hive-packages-oracle.mjs"),
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
        "Package original: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn bind(step: &Value, rows: &[Value]) -> Value {
    let mut step = step.clone();
    for binding in step["bindings"]
        .as_array()
        .into_iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
    {
        let mut value = rows[binding["from"].as_u64().unwrap() as usize]
            .pointer(binding["select"].as_str().unwrap())
            .unwrap()
            .clone();
        if let Some(prefix) = binding["prefix"].as_str() {
            value = format!("{prefix}{}", value.as_str().unwrap()).into();
        }
        if let Some(suffix) = binding["suffix"].as_str() {
            value = format!("{}{suffix}", value.as_str().unwrap()).into();
        }
        *step.pointer_mut(binding["at"].as_str().unwrap()).unwrap() = value;
    }
    step
}
fn projection(grant: &ivy_hive::packages::AuthorizedUpload) -> Value {
    json!({"authorization":wire(grant.authorization().clone()),"principalId":grant.principal_id().to_utf8().unwrap(),"uploadId":grant.upload_id().to_utf8().unwrap()})
}
struct Installer {
    actor: Arc<Actor>,
    root: PathBuf,
    calls: Mutex<Vec<Value>>,
    failure: std::sync::atomic::AtomicBool,
}
impl PackageUiInstaller for Installer {
    fn install(&self, input: PackageUiInstallation) -> PackageInstallation {
        self.calls.lock().unwrap().push(json!({"manifest":wire(input.manifest),"artifactRoot":input.artifact_root.strip_prefix(&self.root).unwrap().to_str().unwrap().replace('\\',"/"),"publisherPrincipalId":input.publisher_principal_id.to_utf8().unwrap(),"buildId":input.build_id.to_utf8().unwrap(),"files":ui_files::tree(&input.artifact_root)}));
        self.actor.control(json!({"method":"install"}));
        let result = if self.failure.load(Ordering::SeqCst) {
            Err(IvyError::new(
                "fixture_failure",
                "Fixture installer failure.",
            ))
        } else {
            Ok(())
        };
        Box::pin(std::future::ready(result))
    }
}
struct Body {
    chunks: Vec<Vec<u8>>,
    offset: usize,
    failure: bool,
}
impl AsyncRead for Body {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.chunks.is_empty() {
            return std::task::Poll::Ready(if self.failure {
                Err(std::io::Error::other("Fixture stream failed."))
            } else {
                Ok(())
            });
        }
        let length = buffer.remaining().min(self.chunks[0].len() - self.offset);
        buffer.put_slice(&self.chunks[0][self.offset..self.offset + length]);
        self.offset += length;
        if self.offset == self.chunks[0].len() {
            self.chunks.remove(0);
            self.offset = 0;
        }
        std::task::Poll::Ready(Ok(()))
    }
}
fn compare(steps: &[Value]) -> Value {
    compare_roots(steps, false)
}
fn compare_roots(steps: &[Value], namespaced: bool) -> Value {
    let root = Root::new();
    let files = root.0.join(if namespaced {
        "native-files. "
    } else {
        "native-files"
    });
    let reference = root.0.join(if namespaced {
        "reference-files. "
    } else {
        "reference-files"
    });
    let mut dates = steps
        .iter()
        .flat_map(|s| {
            let now = s["now"].as_i64().unwrap_or(100000);
            [now, now + 300000]
        })
        .collect::<Vec<_>>();
    dates.sort_unstable();
    dates.dedup();
    let iso = node(json!({"dates":dates}));
    let clock = Arc::new(Clock {
        now: AtomicI64::new(100000),
        iso: iso
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p[0].as_i64().unwrap(), p[1].as_str().unwrap().into()))
            .collect(),
    });
    let (actor, thread) = Actor::start(root.0.join("native.sqlite"), clock.clone());
    let installer = Arc::new(Installer {
        actor: actor.clone(),
        root: files.clone(),
        calls: Mutex::new(vec![]),
        failure: std::sync::atomic::AtomicBool::new(false),
    });
    let construct = |missing, install| {
        PackageRegistry::new(
            &files,
            "/hive".into(),
            clock.clone(),
            if missing {
                None
            } else {
                Some(actor.clone() as Arc<dyn PackageStorage>)
            },
            PackageOptions {
                install_ui: if install {
                    Some(installer.clone())
                } else {
                    None
                },
                ..Default::default()
            },
        )
    };
    let mut registry = construct(false, false);
    let mut staged: HashMap<String, StagedPackageUpload> = HashMap::new();
    let mut actual = vec![];
    let mut original = steps.to_vec();
    let mut previous_mutations = 0;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for (index, source) in steps.iter().enumerate() {
        let step = bind(source, &actual);
        clock
            .now
            .store(step["now"].as_i64().unwrap_or(100000), Ordering::SeqCst);
        let outcome: IvyResult<Value>=runtime.block_on(async {
            match step["method"].as_str().unwrap() {
                "fixture" => { ui_files::fixture(&files,&step["actions"]); Ok(Value::Null) },
                "sql" | "reopen" => { actor.control(step.clone()); if step["method"]=="reopen" { registry=construct(step["missingWorker"]==true,step["installer"]==true); } Ok(Value::Null) },
                "failNext" => { *actor.failure.lock().unwrap()=Some(step["failure"].clone()); Ok(Value::Null) },
                "construct" => { registry=construct(step["missingWorker"]==true,step["installer"]==true); Ok(Value::Null) },
                "installFailure" => { installer.failure.store(step["enabled"]==true,Ordering::SeqCst);Ok(Value::Null) },
                "initialize" => registry.initialize().await.map(|_|Value::Null),
                "catalog" | "snapshot" => {
                    let after=IvyJson::from_serde(step.get("after").unwrap_or(&json!(0))).unwrap(); let include=step["includeUis"].as_bool().unwrap_or(true);
                    if step["method"]=="catalog" { registry.catalog(&after,include).await.map(wire) } else { registry.catalog_snapshot(&after,include).await.map(wire) }
                },
                "authorize" | "authorizeMany" => {
                    let input=IvyJson::from_serde(&step["input"]).unwrap(); let principal=step["principal"].as_str().unwrap_or("publisher").into();
                    if step["method"]=="authorize" { registry.authorize(&principal,&input).await.map(wire) }
                    else { let mut results=vec![]; for _ in 0..step["count"].as_u64().unwrap() { results.push(wire(registry.authorize(&principal,&input).await?)); } Ok(json!(results)) }
                },
                "admit" => registry.admit_upload(&step["uploadId"].as_str().unwrap().into(),&IvyJson::from_serde(&step["headers"]).unwrap()).map(|v|projection(&v)),
                "stage" => {
                    let grant=registry.admit_upload(&step["uploadId"].as_str().unwrap().into(),&IvyJson::from_serde(&step["headers"]).unwrap())?; let mut value=projection(&grant);
                    let body=Body { chunks:step["chunks"].as_array().unwrap().iter().map(|v|STANDARD.decode(v.as_str().unwrap()).unwrap()).collect(),offset:0,failure:step["readFailure"]==true };
                    let upload=registry.stage_upload(grant,body).await?;
                    value["archive"]=upload.archive().strip_prefix(&files).unwrap().to_str().unwrap().replace('\\',"/").into();
                    value["extraction"]=upload.extraction().strip_prefix(&files).unwrap().to_str().unwrap().replace('\\',"/").into();
                    staged.insert(step["label"].as_str().unwrap().into(),upload); Ok(value)
                },
                "upload" => {
                    let body=Body { chunks:step["chunks"].as_array().unwrap().iter().map(|v|STANDARD.decode(v.as_str().unwrap()).unwrap()).collect(),offset:0,failure:step["readFailure"]==true };
                    registry.upload(&step["uploadId"].as_str().unwrap().into(),&IvyJson::from_serde(&step["headers"]).unwrap(),body).await.map(wire)
                },
                "uploadPair" => {
                    let run=|upload:&Value|{let body=Body {chunks:upload["chunks"].as_array().unwrap().iter().map(|v|STANDARD.decode(v.as_str().unwrap()).unwrap()).collect(),offset:0,failure:false};let id:JsString=upload["uploadId"].as_str().unwrap().into();let headers=IvyJson::from_serde(&upload["headers"]).unwrap();let registry=registry.clone();async move {registry.upload(&id,&headers,body).await.map(wire)}};
                    let (a,b)=tokio::join!(run(&step["uploads"][0]),run(&step["uploads"][1]));Ok(json!([a?,b?]))
                },
                "dropStage" => { staged.remove(step["label"].as_str().unwrap()).unwrap().cleanup().await; Ok(Value::Null) },
                "artifact" => registry.artifact(&step["componentId"].as_str().unwrap().into(),&step["version"].as_str().unwrap().into()).await.map(|(path,entry)|json!({"path":path.strip_prefix(&files).unwrap().to_str().unwrap().replace('\\',"/"),"entry":wire(entry)})),
                "collect" => registry.collect_artifacts().await.map(|_|Value::Null),
                _=>panic!("Unknown package trace method"),
            }
        });
        let (value, error) = match outcome {
            Ok(v) => (v, Value::Null),
            Err(e) => (Value::Null, wire(e.to_wire())),
        };
        let observed = actor.control(json!({"method":"snapshot"}));
        let mut uuids = vec![];
        let mutations = observed["catalogMutations"].as_array().unwrap();
        for (n, mutation) in mutations.iter().enumerate().skip(previous_mutations) {
            let nonce = mutation["mutation_id"]
                .as_str()
                .unwrap()
                .rsplit(':')
                .next()
                .unwrap();
            assert_eq!(uuid::Uuid::parse_str(nonce).unwrap().get_version_num(), 4);
            uuids.push(nonce.to_owned());
            if n == 0 {
                uuids.push(
                    observed["state"]["objects"][0]["id"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                );
            }
        }
        let mut randoms = vec![];
        let authorizations = if step["method"] == "authorizeMany" {
            value.as_array().cloned().unwrap_or_default()
        } else {
            vec![value.clone()]
        };
        for authorization in authorizations {
            if authorization["alreadyPublished"] == false {
                let id = authorization["uploadId"].as_str().unwrap();
                assert_eq!(uuid::Uuid::parse_str(id).unwrap().get_version_num(), 4);
                uuids.push(id.to_owned());
                let token = authorization["uploadToken"].as_str().unwrap();
                let bytes = URL_SAFE_NO_PAD
                    .decode(token.strip_prefix("ivypu1.").unwrap())
                    .unwrap();
                assert_eq!(bytes.len(), 32);
                randoms.push(STANDARD.encode(bytes));
            }
        }
        if uuids.is_empty() {
            uuids.push(format!("00000000-0000-4000-8000-{index:012}"));
            uuids.push(format!("00000000-0001-4000-8000-{index:012}"));
        }
        original[index]["uuids"] = json!(uuids);
        original[index]["randoms"] = json!(randoms);
        previous_mutations = mutations.len();
        actual.push(json!({"value":value,"error":error,"state":observed["state"],"fileTree":ui_files::tree(&files),"requests":actor.requests.lock().unwrap().clone(),"installerCalls":installer.calls.lock().unwrap().clone(),"catalogMutations":observed["catalogMutations"]}));
    }
    let definitions = actor.control(json!({"method":"snapshot"}))["definitions"].clone();
    drop(staged);
    drop(registry);
    drop(installer);
    drop(actor);
    thread.join().unwrap();
    let expected = node(
        json!({"path":root.0.join("reference.sqlite"),"root":reference,"basePath":"/hive","steps":original}),
    );
    assert_eq!(actual.len(), expected["rows"].as_array().unwrap().len());
    for (index, (a, b)) in actual
        .iter()
        .zip(expected["rows"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(a, b, "Package registry step {index}: {}", steps[index]);
        if steps[index]["requireSuccess"] == true
            || matches!(steps[index]["method"].as_str(), Some("fixture" | "sql"))
        {
            assert!(
                a["error"].is_null(),
                "Fixture setup {index}: {}",
                a["error"]
            );
        }
    }
    assert_eq!(definitions, expected["definitions"]);
    if namespaced {
        assert!(!root.0.join("native-files").exists());
        assert!(!root.0.join("reference-files").exists());
    }
    expected
}
fn step(method: &str) -> Value {
    json!({"method":method})
}
fn setup() -> Vec<Value> {
    vec![json!({"method":"initialize","requireSuccess":true})]
}
fn authorization(component: &str, version: &str, kind: &str, bytes: &[u8]) -> Value {
    let build = ivy_contracts::hash::digest(format!("{component}/{version}"));
    let mut manifest = json!({"schemaVersion":1,"componentId":component,"kind":kind,"version":version,"buildId":build,"connectsToHive":false,"requirements":{"node":">=24.18.0 <25.0.0","hiveProtocol":1,"contracts":[]}});
    if kind == "app" {
        manifest["app"] = json!({"appId":component,"dist":format!("dist/apps/{component}"),"entryPath":"index.html"});
    } else {
        manifest["entrypoint"] =
            json!({"executable":"node","args":["dist/main.js"],"timeoutMs":5000});
        manifest["readiness"] = json!({"timeoutMs":5000,"command":{"executable":"node","args":["dist/health.js"],"timeoutMs":5000}});
        manifest["shutdown"] = json!({"timeoutMs":1000});
        manifest["restart"] = json!({"policy":"always","minimumDelayMs":100,"maximumDelayMs":500});
    }
    json!({"componentId":component,"version":version,"buildId":build,"archiveHash":ivy_contracts::hash::digest(bytes),"bytes":bytes.len(),"manifest":manifest})
}
fn authorize(input: Value) -> Value {
    json!({"method":"authorize","input":input,"requireSuccess":true})
}
fn push(steps: &mut Vec<Value>, value: Value) -> usize {
    let index = steps.len();
    steps.push(value);
    index
}
fn headers(method: &str, from: usize) -> Value {
    json!({"method":method,"uploadId":"","headers":{"authorization":"","content-length":"3"},"bindings":[{"at":"/uploadId","from":from,"select":"/value/uploadId"},{"at":"/headers/authorization","from":from,"select":"/value/uploadToken","prefix":"Bearer "}]})
}
fn fixture(actions: Vec<Value>) -> Value {
    json!({"method":"fixture","actions":actions})
}
fn write(path: impl Into<String>, bytes: impl AsRef<[u8]>) -> Value {
    json!({"kind":"write","path":path.into(),"data":STANDARD.encode(bytes)})
}
fn error(result: &Value, index: usize, code: &str) {
    assert_eq!(result["rows"][index]["error"]["data"]["code"], code);
}
fn bundle(component: &str, version: &str, kind: &str, stamp: Value) -> Value {
    let mut authorization = authorization(component, version, kind, b"x");
    let archive =
        node(json!({"buildArchive":{"manifest":authorization["manifest"],"stamp":stamp}}));
    authorization["archiveHash"] = archive["archiveHash"].clone();
    authorization["bytes"] = archive["bytes"].clone();
    json!({"authorization":authorization,"bytesBase64":archive["bytesBase64"]})
}
fn upload(grant: usize, package: &Value) -> Value {
    let mut upload = headers("upload", grant);
    upload["headers"]["content-length"] = package["authorization"]["bytes"]
        .as_u64()
        .unwrap()
        .to_string()
        .into();
    upload["chunks"] = json!([package["bytesBase64"]]);
    upload["requireSuccess"] = true.into();
    upload
}

#[cfg(windows)]
#[test]
fn package_registry_namespaced_root_preserves_logical_staging_cleanup_and_incoming_collection() {
    let mut steps = setup();
    let grant = push(
        &mut steps,
        authorize(authorization("fixture", "1.0.0", "service", b"abc")),
    );
    let mut staged = headers("stage", grant);
    staged["chunks"] = json!([STANDARD.encode(b"abc")]);
    staged["label"] = "staged".into();
    staged["requireSuccess"] = true.into();
    let staged = push(&mut steps, staged);
    let cleaned = push(
        &mut steps,
        json!({"method":"dropStage","label":"staged","requireSuccess":true}),
    );
    let old = "incoming/00000000-0000-4000-8000-000000000001.tar.gz";
    steps.push(fixture(vec![
        write(old, b"old"),
        json!({"kind":"mtime","path":old,"ms":0}),
    ]));
    let collected = push(
        &mut steps,
        json!({"method":"collect","now":172800000,"requireSuccess":true}),
    );
    let result = compare_roots(&steps, true);
    let staged_path = result["rows"][staged]["value"]["archive"].as_str().unwrap();
    let has = |index: usize, path: &str| {
        result["rows"][index]["fileTree"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == path)
    };
    assert!(has(staged, staged_path));
    assert!(!has(cleaned, staged_path));
    assert!(!has(collected, old));
}

#[test]
fn package_registry_actual_archive_publication_concurrent_repeat_retention_and_reopen_match_original()
 {
    for kind in ["service", "app"] {
        let component = format!("fixture-{kind}");
        let packages = (1..=3)
            .map(|v| bundle(&component, &format!("{v}.0.0"), kind, json!({})))
            .collect::<Vec<_>>();
        let mut s = setup();
        if kind == "app" {
            s.push(json!({"method":"construct","installer":true}));
        }
        let first = push(&mut s, authorize(packages[0]["authorization"].clone()));
        let repeat = push(&mut s, authorize(packages[0]["authorization"].clone()));
        let old = push(&mut s, authorize(packages[0]["authorization"].clone()));
        let first_upload = if kind == "service" {
            let uploads = [upload(first, &packages[0]), upload(repeat, &packages[0])];
            let mut pair = json!({"method":"uploadPair","uploads":uploads,"bindings":[],"requireSuccess":true});
            for (index, upload) in uploads.iter().enumerate() {
                for binding in upload["bindings"].as_array().unwrap() {
                    let mut binding = binding.clone();
                    binding["at"] =
                        format!("/uploads/{index}{}", binding["at"].as_str().unwrap()).into();
                    pair["bindings"].as_array_mut().unwrap().push(binding);
                }
            }
            push(&mut s, pair)
        } else {
            let first_upload = push(&mut s, upload(first, &packages[0]));
            let repeat_upload = push(&mut s, upload(repeat, &packages[0]));
            let _ = repeat_upload;
            first_upload
        };
        for package in packages.iter().skip(1) {
            let grant = push(&mut s, authorize(package["authorization"].clone()));
            s.push(upload(grant, package));
        }
        let mut late = upload(old, &packages[0]);
        late["requireSuccess"] = false.into();
        let late = push(&mut s, late);
        let catalog = push(&mut s, step("catalog"));
        s.push(json!({"method":"reopen","installer":kind=="app"}));
        let reopened = push(&mut s, step("catalog"));
        let r = compare(&s);
        error(&r, late, "version_conflict");
        assert_eq!(r["rows"][catalog]["value"], r["rows"][reopened]["value"]);
        let entries = r["rows"][catalog]["value"]["packages"].as_array().unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|e| e["version"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["2.0.0", "3.0.0"]
        );
        if kind == "service" {
            assert_eq!(
                r["rows"][first_upload]["value"][0],
                r["rows"][first_upload]["value"][1]
            );
        } else {
            assert_eq!(
                r["rows"][reopened]["installerCalls"]
                    .as_array()
                    .unwrap()
                    .len(),
                3
            );
        }
        assert!(
            !r["rows"][reopened]["fileTree"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["path"]
                    .as_str()
                    .unwrap()
                    .starts_with(&format!("artifacts/{component}/1.0.0"))
                    || v["path"].as_str().unwrap().contains("-extract"))
        );
    }
}
#[test]
fn package_registry_actual_receiver_refuses_bad_archive_manifest_and_build_before_publication() {
    let good = bundle("fixture-service", "1.0.0", "service", json!({}));
    let bad_stamp = bundle(
        "fixture-service",
        "1.0.0",
        "service",
        json!({"componentId":"different"}),
    );
    let mut malformed = good.clone();
    malformed["bytesBase64"] = STANDARD.encode(b"not an archive").into();
    malformed["authorization"]["archiveHash"] =
        ivy_contracts::hash::digest(b"not an archive").into();
    malformed["authorization"]["bytes"] = 14.into();
    let mut manifest = good.clone();
    manifest["authorization"]["manifest"]["connectsToHive"] = true.into();
    for package in [malformed, bad_stamp, manifest] {
        let mut s = setup();
        let grant = push(&mut s, authorize(package["authorization"].clone()));
        let mut bad = upload(grant, &package);
        bad["requireSuccess"] = false.into();
        let bad = push(&mut s, bad);
        let reused = push(&mut s, headers("admit", grant));
        let r = compare(&s);
        assert!(!r["rows"][bad]["error"].is_null());
        error(&r, reused, "unauthenticated");
        assert!(
            !r["rows"][bad]["requests"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["action"] == "package.publish")
        );
        assert_eq!(
            r["rows"][bad]["fileTree"],
            json!([{"path":"incoming","kind":"directory"}])
        );
    }
}
#[test]
fn package_registry_publication_sql_rollback_cleans_promoted_files_retains_installer_effect_and_retries()
 {
    for kind in ["service", "app"] {
        let package = bundle(&format!("fixture-{kind}"), "1.0.0", kind, json!({}));
        let mut s = setup();
        if kind == "app" {
            s.push(json!({"method":"construct","installer":true}));
        }
        s.push(json!({"method":"sql","sql":"CREATE TRIGGER fixture_package_failure BEFORE INSERT ON revisions WHEN NEW.revision>1 BEGIN SELECT RAISE(ABORT,'fixture'); END;"}));
        let grant = push(&mut s, authorize(package["authorization"].clone()));
        let mut failed = upload(grant, &package);
        failed["requireSuccess"] = false.into();
        let failed = push(&mut s, failed);
        s.push(json!({"method":"sql","sql":"DROP TRIGGER fixture_package_failure;"}));
        let grant = push(&mut s, authorize(package["authorization"].clone()));
        let retried = push(&mut s, upload(grant, &package));
        let r = compare(&s);
        error(&r, failed, "internal_error");
        assert_eq!(
            r["rows"][failed]["fileTree"],
            json!([{"path":"artifacts","kind":"directory"},{"path":"incoming","kind":"directory"}])
        );
        assert!(r["rows"][retried]["error"].is_null());
        if kind == "app" {
            assert_eq!(
                r["rows"][failed]["installerCalls"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                r["rows"][retried]["installerCalls"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
        }
    }
}
#[test]
fn package_registry_retained_archive_and_ui_installer_boundaries_match_original() {
    let package = bundle("fixture-service", "1.0.0", "service", json!({}));
    let path = format!(
        "artifacts/fixture-service/1.0.0/{}.tar.gz",
        &package["authorization"]["archiveHash"].as_str().unwrap()[7..]
    );
    for valid in [false, true] {
        let mut s = setup();
        s.push(fixture(vec![write(
            &path,
            if valid {
                STANDARD
                    .decode(package["bytesBase64"].as_str().unwrap())
                    .unwrap()
            } else {
                vec![b'x'; package["authorization"]["bytes"].as_u64().unwrap() as usize]
            },
        )]));
        let grant = push(&mut s, authorize(package["authorization"].clone()));
        let mut uploaded = upload(grant, &package);
        uploaded["requireSuccess"] = valid.into();
        let uploaded = push(&mut s, uploaded);
        let r = compare(&s);
        if valid {
            assert!(r["rows"][uploaded]["error"].is_null());
        } else {
            error(&r, uploaded, "artifact_changed");
        }
    }
    let package = bundle("fixture-app", "1.0.0", "app", json!({}));
    let mut s = setup();
    let grant = push(&mut s, authorize(package["authorization"].clone()));
    let mut missing = upload(grant, &package);
    missing["requireSuccess"] = false.into();
    let missing = push(&mut s, missing);
    s.push(json!({"method":"construct","installer":true}));
    s.push(json!({"method":"installFailure","enabled":true}));
    let grant = push(&mut s, authorize(package["authorization"].clone()));
    let mut failed = upload(grant, &package);
    failed["requireSuccess"] = false.into();
    let failed = push(&mut s, failed);
    s.push(json!({"method":"installFailure","enabled":false}));
    let grant = push(&mut s, authorize(package["authorization"].clone()));
    let retried = push(&mut s, upload(grant, &package));
    let r = compare(&s);
    error(&r, missing, "service_unavailable");
    error(&r, failed, "fixture_failure");
    assert!(r["rows"][retried]["error"].is_null());
    assert_eq!(
        r["rows"][retried]["installerCalls"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn package_registry_initialization_worker_guards_legacy_seed_reopen_and_filters_match_original() {
    let auth = authorization("service", "1.0.0", "service", b"abc");
    let mut entry = auth.clone();
    entry["revision"] = 5.into();
    entry["publishedAt"] = "1970-01-01T00:00:00.000Z".into();
    entry["publisherPrincipalId"] = "publisher".into();
    let mut steps = vec![
        json!({"method":"construct","missingWorker":true}),
        step("initialize"),
        step("snapshot"),
        fixture(vec![write(
            "catalog.json",
            json!({"schemaVersion":1,"revision":5,"packages":[entry]}).to_string(),
        )]),
        step("construct"),
        json!({"method":"initialize","requireSuccess":true}),
        step("snapshot"),
        json!({"method":"catalog","after":5}),
        fixture(vec![write("catalog.json", b"invalid obsolete seed")]),
        step("initialize"),
        step("reopen"),
        step("snapshot"),
    ];
    let filter = push(&mut steps, json!({"method":"catalog","after":-1}));
    let r = compare(&steps);
    error(&r, 1, "service_unavailable");
    error(&r, 2, "service_unavailable");
    error(&r, filter, "invalid_arguments");
    assert_eq!(r["rows"][7]["value"]["packages"], json!([]));
    assert_eq!(r["rows"][6]["value"], r["rows"][11]["value"]);
    for data in [
        b"{".as_slice(),
        b"\xef\xbb\xbf{}".as_slice(),
        b"[]".as_slice(),
    ] {
        let r = compare(&[
            fixture(vec![write("catalog.json", data)]),
            step("initialize"),
        ]);
        assert!(!r["rows"][1]["error"].is_null());
    }
    let r = compare(&[
        json!({"method":"failNext","failure":{"action":"package.catalog","code":"storage_invalid"}}),
        step("initialize"),
    ]);
    error(&r, 1, "storage_invalid");
    assert_eq!(r["rows"][1]["requests"].as_array().unwrap().len(), 1);
    let r = compare(&[
        json!({"method":"construct","missingWorker":true}),
        step("collect"),
    ]);
    error(&r, 1, "internal_error");
}
#[test]
fn package_registry_grants_capacity_expiry_one_time_headers_and_restart_match_original() {
    let mut steps = setup();
    let auth = authorization("service", "1.0.0", "service", b"abc");
    let first = push(&mut steps, authorize(auth.clone()));
    let mut wrong = headers("admit", first);
    wrong["headers"]["authorization"] = "Bearer ivypu1.wrong".into();
    wrong["bindings"].as_array_mut().unwrap().pop();
    let wrong = push(&mut steps, wrong);
    let mut encoded = headers("admit", first);
    encoded["headers"]["content-encoding"] = "identity".into();
    let encoded = push(&mut steps, encoded);
    let reused = push(&mut steps, headers("admit", first));
    let next = push(&mut steps, authorize(auth.clone()));
    let mut zeroes = headers("admit", next);
    zeroes["headers"]["content-length"] = "0003".into();
    zeroes["requireSuccess"] = true.into();
    push(&mut steps, zeroes);
    let many = push(
        &mut steps,
        json!({"method":"authorizeMany","input":auth,"count":128,"requireSuccess":true}),
    );
    let mut full = authorize(authorization("another", "1.0.0", "service", b"abc"));
    full["requireSuccess"] = false.into();
    let full = push(&mut steps, full);
    let mut expired = headers("admit", first);
    expired["now"] = 400000.into();
    let expired = push(&mut steps, expired);
    let mut freed = authorize(authorization("service", "2.0.0", "service", b"abc"));
    freed["now"] = 400000.into();
    let freed = push(&mut steps, freed);
    steps.push(step("reopen"));
    let mut lost = headers("admit", freed);
    lost["now"] = 400000.into();
    let lost = push(&mut steps, lost);
    let r = compare(&steps);
    error(&r, wrong, "unauthenticated");
    error(&r, encoded, "invalid_arguments");
    error(&r, reused, "unauthenticated");
    error(&r, full, "capacity_exceeded");
    error(&r, expired, "unauthenticated");
    error(&r, lost, "unauthenticated");
    assert_eq!(r["rows"][many]["value"].as_array().unwrap().len(), 128);
    assert!(r["rows"][freed]["error"].is_null());
}
#[test]
fn package_registry_header_type_and_consumption_boundaries_match_original() {
    for (field, value, expected) in [
        ("authorization", json!(null), "unauthenticated"),
        (
            "authorization",
            json!(["Bearer ivypu1.bad"]),
            "unauthenticated",
        ),
        (
            "authorization",
            json!("bearer ivypu1.bad"),
            "unauthenticated",
        ),
        ("content-length", json!(3), "invalid_arguments"),
        ("content-length", json!("3.0"), "invalid_arguments"),
        ("content-length", json!("+3"), "invalid_arguments"),
        ("content-length", json!(" 3"), "invalid_arguments"),
        ("content-length", json!(["3"]), "invalid_arguments"),
        ("content-encoding", json!(null), "invalid_arguments"),
    ] {
        let mut s = setup();
        let grant = push(
            &mut s,
            authorize(authorization("service", "1.0.0", "service", b"abc")),
        );
        let mut h = headers("admit", grant);
        h["headers"][field] = value;
        if field == "authorization" {
            h["bindings"].as_array_mut().unwrap().pop();
        }
        let bad = push(&mut s, h);
        let retry = push(&mut s, headers("admit", grant));
        let r = compare(&s);
        error(&r, bad, expected);
        if field == "authorization" {
            assert!(r["rows"][retry]["error"].is_null());
        } else {
            error(&r, retry, "unauthenticated");
        }
    }
}
#[test]
fn package_registry_staging_stream_hash_size_exclusive_create_cleanup_and_lost_grants_match_original()
 {
    for (body, read_failure, expected) in [
        (b"abc".as_slice(), false, None),
        (b"ab".as_slice(), false, Some("artifact_changed")),
        (b"abd".as_slice(), false, Some("artifact_changed")),
        (b"abcd".as_slice(), false, Some("content_too_large")),
        (b"ab".as_slice(), true, Some("internal_error")),
    ] {
        let mut s = setup();
        let grant = push(
            &mut s,
            authorize(authorization("service", "1.0.0", "service", b"abc")),
        );
        let mut upload = headers("stage", grant);
        upload["chunks"] = json!([STANDARD.encode(body)]);
        upload["label"] = "body".into();
        upload["readFailure"] = read_failure.into();
        let result = push(&mut s, upload);
        let reused = push(&mut s, headers("admit", grant));
        if expected.is_none() {
            s.push(json!({"method":"dropStage","label":"body","requireSuccess":true}));
        }
        let r = compare(&s);
        if let Some(code) = expected {
            error(&r, result, code);
        } else {
            assert!(r["rows"][result]["error"].is_null());
        }
        error(&r, reused, "unauthenticated");
        assert_eq!(
            r["rows"].as_array().unwrap().last().unwrap()["fileTree"],
            json!([{"path":"incoming","kind":"directory"}])
        );
    }
    for directory in [false, true] {
        let mut s = setup();
        let grant = push(
            &mut s,
            authorize(authorization("service", "1.0.0", "service", b"abc")),
        );
        let mut collision = fixture(vec![
            if directory {
                json!({"kind":"mkdir","path":""})
            } else {
                write("", b"retained collision")
            },
            write("", b"extracted partial"),
        ]);
        collision["bindings"] = json!([{"at":"/actions/0/path","from":grant,"select":"/value/uploadId","prefix":"incoming/","suffix":".tar.gz"},{"at":"/actions/1/path","from":grant,"select":"/value/uploadId","prefix":"incoming/","suffix":"-extract/partial"}]);
        s.push(collision);
        let mut upload = headers("stage", grant);
        upload["chunks"] = json!([STANDARD.encode(b"abc")]);
        upload["label"] = "collision".into();
        let failed = push(&mut s, upload);
        let r = compare(&s);
        error(&r, failed, "internal_error");
        let tree = r["rows"][failed]["fileTree"].as_array().unwrap();
        assert_eq!(tree.len(), if directory { 2 } else { 1 });
    }
    let bytes = vec![b'x'; 196609];
    let mut s = setup();
    let grant = push(
        &mut s,
        authorize(authorization("service", "1.0.0", "service", &bytes)),
    );
    let mut upload = headers("stage", grant);
    upload["headers"]["content-length"] = bytes.len().to_string().into();
    upload["chunks"] = json!(
        bytes
            .chunks(32769)
            .map(|v| STANDARD.encode(v))
            .collect::<Vec<_>>()
    );
    upload["label"] = "large".into();
    upload["requireSuccess"] = true.into();
    s.push(upload);
    s.push(json!({"method":"dropStage","label":"large","requireSuccess":true}));
    compare(&s);
}
#[test]
fn package_registry_latest_identity_repeat_before_capacity_and_version_conflicts_match_original() {
    let mut entry = authorization("service", "2.0.0", "service", b"abc");
    entry["revision"] = 1.into();
    entry["publishedAt"] = "1970-01-01T00:00:00.000Z".into();
    entry["publisherPrincipalId"] = "publisher".into();
    let mut s = vec![
        fixture(vec![write(
            "catalog.json",
            json!({"schemaVersion":1,"revision":1,"packages":[entry.clone()]}).to_string(),
        )]),
        json!({"method":"initialize","requireSuccess":true}),
        json!({"method":"authorizeMany","input":authorization("other","1.0.0","service",b"abc"),"count":128,"requireSuccess":true}),
    ];
    let mut repeat = authorization("service", "2.0.0", "service", b"abc");
    repeat["bytes"] = 1.into();
    let repeat = push(&mut s, authorize(repeat));
    let mut changed = authorize(authorization("service", "2.0.0", "service", b"different"));
    changed["requireSuccess"] = false.into();
    let changed = push(&mut s, changed);
    let mut older = authorize(authorization("service", "1.0.0", "service", b"abc"));
    older["requireSuccess"] = false.into();
    let older = push(&mut s, older);
    let mut newer = authorize(authorization("service", "3.0.0", "service", b"abc"));
    newer["requireSuccess"] = false.into();
    let newer = push(&mut s, newer);
    let r = compare(&s);
    assert_eq!(
        r["rows"][repeat]["value"],
        json!({"alreadyPublished":true,"entry":entry})
    );
    error(&r, changed, "version_conflict");
    error(&r, older, "version_conflict");
    error(&r, newer, "capacity_exceeded");
}
#[test]
fn package_registry_artifact_collection_preserves_current_unknown_and_linked_files_matches_original()
 {
    let mut entry = authorization("retained", "2.0.0", "service", b"abc");
    entry["revision"] = 1.into();
    entry["publishedAt"] = "1970-01-01T00:00:00.000Z".into();
    entry["publisherPrincipalId"] = "publisher".into();
    let hash = entry["archiveHash"].as_str().unwrap()[7..].to_owned();
    let incoming = "00000000-0000-4000-8000-000000000001.tar.gz";
    let actions = vec![
        write(
            "catalog.json",
            json!({"schemaVersion":1,"revision":1,"packages":[entry]}).to_string(),
        ),
        write(format!("artifacts/retained/2.0.0/{hash}.tar.gz"), b"abc"),
        write("artifacts/retained/.package", b""),
        write("artifacts/retained/1.0.0/old", b"old"),
        write("artifacts/retained/01.0.0/unknown", b"keep"),
        write("artifacts/orphan/.app", b""),
        write("artifacts/orphan/1.0.0/old", b"old"),
        write("artifacts/unmarked/1.0.0/file", b"keep"),
        write("artifacts/with-note/.package", b""),
        write("artifacts/with-note/README", b"keep"),
        write(format!("incoming/{incoming}"), b"stale"),
        json!({"kind":"mtime","path":format!("incoming/{incoming}"),"ms":0}),
        write(
            "incoming/00000000-0000-4000-8000-000000000002.tar.gz",
            b"fresh",
        ),
        write(
            "incoming/00000000-0000-4000-8000-000000000003.TAR.GZ",
            b"unknown",
        ),
        json!({"kind":"mkdir","path":"incoming/00000000-0000-4000-8000-000000000004-extract"}),
        json!({"kind":"mtime","path":"incoming/00000000-0000-4000-8000-000000000004-extract","ms":1}),
        write("outside/keep", b"outside"),
        json!({"kind":"junction","path":"artifacts/unreferenced-link","target":"outside"}),
        json!({"kind":"junction","path":"artifacts/retained/0.0.0","target":"outside"}),
    ];
    let s = vec![
        fixture(actions),
        json!({"method":"initialize","now":86400000,"requireSuccess":true}),
        json!({"method":"artifact","componentId":"retained","version":"2.0.0","requireSuccess":true}),
        json!({"method":"artifact","componentId":"retained","version":"1.0.0"}),
        fixture(vec![write(
            format!("artifacts/retained/2.0.0/{hash}.tar.gz"),
            b"abd",
        )]),
        json!({"method":"artifact","componentId":"retained","version":"2.0.0","requireSuccess":true}),
        fixture(vec![write(
            format!("artifacts/retained/2.0.0/{hash}.tar.gz"),
            b"larger",
        )]),
        json!({"method":"artifact","componentId":"retained","version":"2.0.0"}),
        step("collect"),
    ];
    let r = compare(&s);
    error(&r, 3, "not_found");
    error(&r, 7, "artifact_changed");
    assert!(r["rows"][5]["error"].is_null());
    let tree = r["rows"][1]["fileTree"].as_array().unwrap();
    assert!(!tree.iter().any(|v| v["path"] == "artifacts/orphan"
        || v["path"] == format!("incoming/{incoming}")
        || v["path"] == "artifacts/retained/1.0.0"));
    assert!(
        tree.iter()
            .any(|v| v["path"] == "incoming/00000000-0000-4000-8000-000000000004-extract")
    );
}
#[test]
fn package_registry_collection_failure_releases_chain_and_checks_markers_before_removing_files() {
    let mut s = setup();
    s.push(fixture(vec![
        write("artifacts/orphan/.package", b""),
        write("artifacts/orphan/1.0.0/old", b"old"),
    ]));
    s.push(
        json!({"method":"failNext","failure":{"action":"package.prune","code":"fixture_failure"}}),
    );
    let failed = push(&mut s, step("collect"));
    let retried = push(&mut s, json!({"method":"collect","requireSuccess":true}));
    let r = compare(&s);
    error(&r, failed, "fixture_failure");
    assert!(
        r["rows"][failed]["fileTree"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["path"] == "artifacts/orphan/1.0.0/old")
    );
    assert!(
        !r["rows"][retried]["fileTree"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["path"] == "artifacts/orphan")
    );
    for marker in [".app", ".package"] {
        let mut s = setup();
        s.push(fixture(vec![
            json!({"kind":"mkdir","path":format!("artifacts/invalid/{marker}")}),
            write("artifacts/invalid/1.0.0/keep", b"keep"),
        ]));
        let failed = push(&mut s, step("collect"));
        let again = push(&mut s, step("collect"));
        let r = compare(&s);
        error(&r, failed, "storage_invalid");
        error(&r, again, "storage_invalid");
        assert_eq!(r["rows"][failed]["fileTree"], r["rows"][again]["fileTree"]);
    }
}

#[tokio::test]
async fn package_registry_dropped_callers_keep_collection_order_and_stream_without_holding_database()
 {
    use std::{
        future::Future,
        sync::atomic::{AtomicBool, AtomicUsize},
        task::Poll,
        time::Duration,
    };
    use tokio::{io::AsyncWriteExt, sync::Notify};
    #[derive(Clone)]
    struct Gate {
        actor: Arc<Actor>,
        enabled: Arc<AtomicBool>,
        prunes: Arc<AtomicUsize>,
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }
    impl PackageStorage for Gate {
        fn request(&self, request: IvyJson) -> PackageRequest {
            let this = self.clone();
            Box::pin(async move {
                if request.get("action") == Some(&"package.prune".into())
                    && this.enabled.load(Ordering::SeqCst)
                {
                    let count = this.prunes.fetch_add(1, Ordering::SeqCst);
                    if count == 0 {
                        this.entered.notify_one();
                        this.release.notified().await;
                    }
                }
                this.actor.request(request).await
            })
        }
    }
    let root = Root::new();
    let clock = Arc::new(Clock {
        now: AtomicI64::new(100000),
        iso: HashMap::from([
            (100000, "1970-01-01T00:01:40.000Z".into()),
            (400000, "1970-01-01T00:06:40.000Z".into()),
        ]),
    });
    let (actor, thread) = Actor::start(root.0.join("native.sqlite"), clock.clone());
    let gate = Arc::new(Gate {
        actor: actor.clone(),
        enabled: Arc::new(AtomicBool::new(false)),
        prunes: Arc::new(AtomicUsize::new(0)),
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    });
    let registry = PackageRegistry::new(
        root.0.join("files"),
        "/hive".into(),
        clock,
        Some(gate.clone()),
        PackageOptions::default(),
    );
    registry.initialize().await.unwrap();
    gate.enabled.store(true, Ordering::SeqCst);
    let first = registry.clone();
    let first = tokio::spawn(async move { first.collect_artifacts().await });
    gate.entered.notified().await;
    let mut dropped = Box::pin(registry.collect_artifacts());
    std::future::poll_fn(|cx| {
        assert!(matches!(dropped.as_mut().poll(cx), Poll::Pending));
        Poll::Ready(())
    })
    .await;
    drop(dropped);
    let third = registry.clone();
    let third = tokio::spawn(async move { third.collect_artifacts().await });
    registry.catalog(&IvyJson::Number(0.0), true).await.unwrap();
    assert_eq!(gate.prunes.load(Ordering::SeqCst), 1);
    gate.release.notify_one();
    first.await.unwrap().unwrap();
    third.await.unwrap().unwrap();
    assert_eq!(gate.prunes.load(Ordering::SeqCst), 3);
    let authorization =
        IvyJson::from_serde(&authorization("service", "1.0.0", "service", b"abcdef")).unwrap();
    let grant = registry
        .authorize(&"publisher".into(), &authorization)
        .await
        .unwrap();
    let upload_id = grant.get("uploadId").unwrap().as_js_string().unwrap();
    let token = grant.get("uploadToken").unwrap().as_str().unwrap();
    let admitted = registry
        .admit_upload(
            upload_id,
            &IvyJson::object([
                ("authorization", format!("Bearer {token}").into()),
                ("content-length", "6".into()),
            ]),
        )
        .unwrap();
    let archive = registry
        .root()
        .join("incoming")
        .join(format!("{}.tar.gz", upload_id.to_utf8().unwrap()));
    let (mut sender, body) = tokio::io::duplex(1);
    let mut dropped = Box::pin(registry.stage_upload(admitted, body));
    std::future::poll_fn(|cx| {
        assert!(matches!(dropped.as_mut().poll(cx), Poll::Pending));
        Poll::Ready(())
    })
    .await;
    drop(dropped);
    sender.write_all(b"abc").await.unwrap();
    assert!(archive.exists());
    registry.catalog(&IvyJson::Number(0.0), true).await.unwrap();
    sender.write_all(b"def").await.unwrap();
    sender.shutdown().await.unwrap();
    drop(sender);
    tokio::time::timeout(Duration::from_secs(5), async {
        while archive.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Dropped staging caller retained leaked bytes");
    drop(registry);
    drop(gate);
    drop(actor);
    thread.join().unwrap();
}
