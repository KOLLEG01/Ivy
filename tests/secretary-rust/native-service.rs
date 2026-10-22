//! Run the native executable against a loopback Hive protocol peer, without Node.
use futures_util::{SinkExt, StreamExt};
use ivy_contracts::{
    hash::{hash_json, tool_definition_hash},
    json::{canonical, parse},
};
use ivy_secretary::{
    assignment_schema::default_configuration,
    store::{Record, record_references},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
    time::{sleep, timeout},
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const ROOT: &str = "00000000-0000-4000-8000-000000000001";
const CONFIGURATION: &str = "00000000-0000-4000-8000-000000000099";
const AT: &str = "2026-01-01T00:00:00.000Z";

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn write(path: impl AsRef<Path>, value: &Value) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}
fn read(path: impl AsRef<Path>) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}
fn settings() -> Value {
    json!({
        "identity": {"scope": {"secretaryId":"secretary", "rootObjectId":ROOT},
            "principalId":"secretary-principal", "hostId":"fixture-host", "serviceNodeId":"fixture.secretary"},
        "sources":[], "policy":{"silentTime":null,"urgentBypass":false,"whatsappQuestionsOnly":false,
            "minimumMainUrgency":"normal","researchMaxMinutes":0,"voiceEscalation":null},
        "pollMs":100,"recordsPerTick":10
    })
}
fn configuration(workspace: &Path) -> Value {
    let settings = parse(&settings().to_string(), 1024 * 1024).unwrap();
    let execution = parse(
        &json!({"serviceNodeId":"fixture.agent","threadCwd":workspace,
        "model":null,"effort":"medium","permissions":":read-only"})
        .to_string(),
        1024 * 1024,
    )
    .unwrap();
    let value = default_configuration(&settings, &AT.into(), Some(&execution)).unwrap();
    let content: Value = serde_json::from_str(&canonical(&value, 1024 * 1024).unwrap()).unwrap();
    let references: Value = serde_json::from_str(
        &canonical(
            &record_references(Record::Configuration, &value).unwrap(),
            1024 * 1024,
        )
        .unwrap(),
    )
    .unwrap();
    json!({"object":{"id":CONFIGURATION,"parentId":ROOT,"effectivelyArchived":false,"contractKey":"secretary/configuration"},
        "revision":{"revision":1,"contractVersion":"1.1.0","mediaType":"application/json",
            "contentHash":hash_json(&value).unwrap(),"byteLength":canonical(&value, 1024 * 1024).unwrap().len(),
            "references":references,"createdAt":AT},"content":{"encoding":"json","value":content}})
}

#[derive(Default)]
struct Observed {
    registry: Option<Value>,
    connected: Option<Value>,
    assignment_queries: usize,
    ready: bool,
    unexpected: Vec<String>,
}
struct Invocation {
    name: &'static str,
    arguments: Value,
    reply: oneshot::Sender<Value>,
}
async fn invoke(sender: &mpsc::Sender<Invocation>, name: &'static str, arguments: Value) -> Value {
    let (reply, result) = oneshot::channel();
    sender
        .send(Invocation {
            name,
            arguments,
            reply,
        })
        .await
        .unwrap();
    let frame = timeout(Duration::from_secs(10), result)
        .await
        .expect("Public call timed out")
        .unwrap();
    assert!(frame.get("error").is_none(), "{frame}");
    frame["result"].clone()
}
async fn peer(
    listener: TcpListener,
    document: Value,
    observed: Arc<Mutex<Observed>>,
    mut calls: mpsc::Receiver<Invocation>,
) {
    let (stream, _) = listener.accept().await.unwrap();
    let mut socket = accept_async(stream).await.unwrap();
    let mut hashes = HashMap::new();
    let mut pending = HashMap::new();
    let mut next_id = 0;
    loop {
        tokio::select! {
            call = calls.recv() => {
                let Some(call) = call else { break; };
                next_id += 1;
                let id = format!("native-{next_id}");
                let hash = hashes.get(call.name).expect("Registered public tool");
                let frame = json!({"jsonrpc":"2.0","id":id,"method":"provider.invoke","params":{
                    "generation":7,"qualifiedName":format!("secretary.{}",call.name),"definitionHash":hash,
                    "callerPrincipalId":"fixture-user","arguments":call.arguments}});
                pending.insert(id, call.reply);
                socket.send(Message::Text(frame.to_string().into())).await.unwrap();
            }
            message = socket.next() => {
                let Some(Ok(Message::Text(text))) = message else { break; };
                let frame: Value = serde_json::from_str(&text).unwrap();
                let Some(method) = frame["method"].as_str() else {
                    if let Some(reply) = frame["id"].as_str().and_then(|id|pending.remove(id)) { let _ = reply.send(frame); }
                    continue;
                };
                let params = &frame["params"];
                let result = match method {
                    "service.connect" => { observed.lock().unwrap().connected = Some(params.clone()); json!({"generation":7}) }
                    "registry.sync" => {
                        for namespace in params["namespaces"].as_array().expect("Namespaces") {
                            for definition in namespace["tools"].as_array().unwrap() {
                                let value = parse(&definition.to_string(), 1024 * 1024).unwrap();
                                hashes.insert(definition["name"].as_str().unwrap().to_owned(), tool_definition_hash(&value).unwrap());
                            }
                        }
                        observed.lock().unwrap().registry = Some(params.clone());
                        json!({})
                    }
                    "service.heartbeat" => { observed.lock().unwrap().ready = params["ready"] == true; json!({"ready":params["ready"]}) }
                    "system.status" => json!({"runtimeEpoch":"native-reference"}),
                    "serviceNodes.get" => json!({"connected":true,"synced":true,"principalId":"secretary-principal","hostId":"fixture-host","serviceName":"secretary"}),
                    "objects.stat" => json!({"effectivelyArchived":false}),
                    "objects.query" => {
                        if params["contractKey"] == "secretary/assignment" { observed.lock().unwrap().assignment_queries += 1; }
                        json!({"items":if params["contractKey"] == "secretary/configuration" {vec![json!({"objectId":CONFIGURATION,"revision":1})]} else {vec![]},"nextCursor":null})
                    }
                    "objects.read" => document.clone(),
                    _ => { observed.lock().unwrap().unexpected.push(method.to_owned()); json!(null) }
                };
                if !frame["id"].is_null() {
                    socket.send(Message::Text(json!({"jsonrpc":"2.0","id":frame["id"],"result":result}).to_string().into())).await.unwrap();
                }
            }
        }
    }
}

fn command(config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ivy-secretary"));
    command
        .env("IVY_INSTANCE_CONFIG", config)
        .env_remove("IVY_NODE_EXECUTABLE")
        .env("PATH", "")
        .env("IVY_LAUNCH_ID", "native-service-test");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}

#[tokio::test]
async fn native_process_registers_serves_and_restarts_without_node_or_job_helper() {
    let directory = Directory(
        std::env::temp_dir().join(format!("ivy-secretary-native-{}", uuid::Uuid::new_v4())),
    );
    let artifact = directory.0.join("artifact");
    let data = directory.0.join("data");
    let instance = directory.0.join("instance.json");
    let build = json!({"version":"fixture","buildId":format!("sha256:{}","a".repeat(64))});
    write(artifact.join("dist/build-info.json"), &build);
    write(
        artifact.join("dist/instructions/secretary-notification-policy.json"),
        &json!("Send useful notifications."),
    );
    let mut config = json!({"schemaVersion":1,"hostId":"fixture-host","instanceId":"fixture.secretary","componentId":"secretary",
        "serviceNodeId":"fixture.secretary","credential":"fixture","publicBaseUrl":"http://127.0.0.1:1",
        "dataRoot":data,"artifactRoot":artifact,"version":"fixture","buildId":build["buildId"],"settings":settings()});

    // Removing Node must not weaken the existing artifact/config identity check.
    config["buildId"] = json!(format!("sha256:{}", "b".repeat(64)));
    write(&instance, &config);
    let rejected = command(&instance).output().unwrap();
    assert!(!rejected.status.success());
    let error: Value = serde_json::from_slice(&rejected.stderr).unwrap();
    assert_eq!(error["code"], "build_mismatch");
    config["buildId"] = build["buildId"].clone();

    let mut previous_boot = Value::Null;
    for _ in 0..2 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        config["publicBaseUrl"] = json!(format!("http://{}", listener.local_addr().unwrap()));
        write(&instance, &config);
        let observed = Arc::new(Mutex::new(Observed::default()));
        let (sender, calls) = mpsc::channel(4);
        let peer = tokio::spawn(peer(
            listener,
            configuration(&directory.0),
            observed.clone(),
            calls,
        ));
        let stderr = directory.0.join("stderr.log");
        let mut process = Process(
            command(&instance)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(fs::File::create(&stderr).unwrap())
                .spawn()
                .unwrap(),
        );
        let health = timeout(Duration::from_secs(15), async {
            loop {
                assert!(
                    process.0.try_wait().unwrap().is_none(),
                    "Service exited: {}",
                    fs::read_to_string(&stderr).unwrap()
                );
                if let Some(health) = read(data.join("health.json")) {
                    if health["ready"] == true && observed.lock().unwrap().ready {
                        break health;
                    }
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("Native service did not become ready");
        assert_ne!(health["bootId"], previous_boot);
        previous_boot = health["bootId"].clone();
        assert_eq!(health["generation"], 7);
        {
            let observed = observed.lock().unwrap();
            assert_eq!(
                observed.connected.as_ref().unwrap()["serviceName"],
                "secretary"
            );
            let tools: usize = observed.registry.as_ref().unwrap()["namespaces"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n["tools"].as_array().unwrap().len())
                .sum();
            assert_eq!(tools, 16);
        }
        let binding = invoke(&sender, "binding", json!({})).await;
        assert_eq!(binding["available"], true);
        assert_eq!(binding["expectedScope"], settings()["identity"]["scope"]);
        let before = observed.lock().unwrap().assignment_queries;
        let assignments = invoke(
            &sender,
            "listAssignments",
            json!({"expectedScope":binding["expectedScope"]}),
        )
        .await;
        assert_eq!(assignments["assignments"], json!([]));
        // A scope mismatch must be rejected at the public service boundary.
        let (reply, result) = oneshot::channel();
        sender
            .send(Invocation {
                name: "listAssignments",
                arguments: json!({"expectedScope":{"secretaryId":"other","rootObjectId":ROOT}}),
                reply,
            })
            .await
            .unwrap();
        let denied = timeout(Duration::from_secs(10), result)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(denied["error"]["data"]["code"], "secretary_scope_conflict");
        timeout(Duration::from_secs(5), async {
            while observed.lock().unwrap().assignment_queries <= before + 1 {
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("Scheduler did not keep polling assignments");
        assert!(data.join("secretary.sqlite").is_file());
        write(
            data.join("control.json"),
            &json!({"schemaVersion":1,"instanceId":"fixture.secretary",
            "launchId":health["launchId"],"bootId":health["bootId"],"action":"shutdown","requestedAt":AT}),
        );
        let status = timeout(Duration::from_secs(10), async {
            loop {
                if let Some(status) = process.0.try_wait().unwrap() {
                    break status;
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("Host shutdown did not finish");
        assert!(status.success());
        assert_eq!(read(data.join("health.json")).unwrap()["ready"], false);
        drop(sender);
        timeout(Duration::from_secs(5), peer)
            .await
            .unwrap()
            .unwrap();
        let observed = observed.lock().unwrap();
        assert!(
            observed.unexpected.is_empty(),
            "Unexpected RPCs: {:?}",
            observed.unexpected
        );
    }
    // Maintenance of a current journal is also native and needs no Hive session.
    let maintenance = command(&instance)
        .args([
            "import",
            "--operation-id",
            "native-reference:1792690000000:import",
        ])
        .output()
        .unwrap();
    assert!(
        maintenance.status.success(),
        "{}",
        String::from_utf8_lossy(&maintenance.stderr)
    );
    let result: Value = serde_json::from_slice(&maintenance.stdout).unwrap();
    assert_eq!(result, json!({"complete":true,"alreadyCurrent":true}));
}
