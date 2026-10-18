use ivy_contracts::{
    IvyError, IvyJson,
    json::{parse, stringify},
};
use ivy_data_collector::{
    engine::{RegisterJob, Runner},
    runner::{McpBridge, McpBridgeFactory, TaskRunner},
    schema,
};
use ivy_sdk::service::ServiceFuture;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("ivy-collector-runner-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn node() -> PathBuf {
    let output = std::process::Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().into()
}
async fn runner(
    root: &Root,
    settings: IvyJson,
    mcp: Option<Arc<dyn McpBridgeFactory>>,
) -> TaskRunner {
    let launcher = if cfg!(windows) {
        Some(
            std::env::var_os("IVY_TEST_JOB_LAUNCHER")
                .expect("qualified Rust job helper")
                .into(),
        )
    } else {
        None
    };
    TaskRunner::new(
        root.0.join("work"),
        root.0.clone(),
        Some(&node()),
        settings,
        launcher,
        mcp,
    )
    .await
    .unwrap()
}
fn task(script: &str) -> IvyJson {
    let mut value=parse(r#"{"id":"fixture","name":"Fixture","enabled":false,"script":"","intervalSeconds":60,"timeoutSeconds":10,"memoryMb":64,"dependencies":{},"config":{},"secretNames":[],"retention":{"maximumCount":1000,"maximumAgeDays":7,"maximumBytes":104857600}}"#,10000).unwrap();
    value.insert("script", script.into());
    schema::parse_schema("taskSchema", &value).unwrap()
}
fn register(root: &Root) -> RegisterJob {
    let path = root.0.join("registered");
    Arc::new(move |name| {
        let path = path.clone();
        Box::pin(async move {
            tokio::fs::write(path, name)
                .await
                .map_err(|_| IvyError::internal())
        })
    })
}
async fn run(runner: &TaskRunner, task: IvyJson, root: &Root) -> Result<IvyJson, IvyError> {
    runner
        .run(
            task,
            IvyJson::Null,
            false.into(),
            CancellationToken::new(),
            register(root),
        )
        .await
}

#[tokio::test]
async fn real_runner_preserves_context_files_exit_codes_and_registration_before_spawn() {
    let root = Root::new();
    let runner = runner(
        &root,
        parse(
            r#"{"secrets":{"TOKEN":"synthetic-only","OTHER":"not-assigned"}}"#,
            10000,
        )
        .unwrap(),
        None,
    )
    .await;
    let mut value = task(
        "import {existsSync,readFileSync} from 'node:fs';export default async c=>({data:{config:c.config,secrets:c.secrets,state:c.state,input:c.input,aborted:c.signal.aborted,privateNode:process.env.IVY_NODE_EXECUTABLE??null,home:process.env.HOME===process.cwd(),registered:process.platform!=='win32'||existsSync('../../registered')},state:null});",
    );
    value.insert("secretNames", IvyJson::Array(vec!["TOKEN".into()]));
    value.insert("config", parse(r#"{"lone":"\ud800"}"#, 100).unwrap());
    let output = run(&runner, value, &root).await.unwrap();
    let data = output.get("data").unwrap();
    assert_eq!(
        data.get("secrets"),
        Some(&parse(r#"{"TOKEN":"synthetic-only"}"#, 100).unwrap())
    );
    assert_eq!(
        data.get("config"),
        Some(&parse(r#"{"lone":"\ud800"}"#, 100).unwrap())
    );
    for key in ["home", "registered"] {
        assert_eq!(data.get(key), Some(&true.into()));
    }
    assert_eq!(data.get("privateNode"), Some(&IvyJson::Null));
    assert_eq!(data.get("input"), Some(&false.into()));
    assert_eq!(data.get("state"), Some(&IvyJson::Null));
    assert_eq!(output.get("state"), Some(&IvyJson::Null));
    assert!(!root.0.join("work/fixture/result.json").exists());
    for (code, expected) in [
        ("authentication_required", "authentication_required"),
        ("interaction_required", "interaction_required"),
        ("configuration_invalid", "configuration_invalid"),
        ("provider_unavailable", "provider_unavailable"),
        ("untrusted-text", "task_failed"),
    ] {
        // A stale output never converts a failing script into a successful run.
        std::fs::write(
            root.0.join("work/fixture/result.json"),
            r#"{"data":"stale"}"#,
        )
        .unwrap();
        let error=run(&runner,task(&format!("export default async()=>{{throw Object.assign(new Error('private provider details'),{{code:'{code}'}})}}")),&root).await.unwrap_err();
        assert_eq!(error.code, expected);
        assert!(!error.message.contains("private provider"));
        assert!(!root.0.join("work/fixture/result.json").exists());
    }
    for script in [
        "export default async()=>undefined;",
        "export default async()=>({data:'x'.repeat(1000000)});",
    ] {
        assert_eq!(
            run(&runner, task(script), &root).await.unwrap_err().code,
            "limit_exceeded"
        );
    }
    let mut missing = task("export default async()=>({data:0})");
    missing.insert("secretNames", IvyJson::Array(vec!["MISSING".into()]));
    assert_eq!(
        run(&runner, missing, &root).await.unwrap_err().code,
        "secret_missing"
    );
    let auth = root.0.join("work/fixture/.auth");
    std::fs::create_dir_all(&auth).unwrap();
    std::fs::write(auth.join("token"), "fixture").unwrap();
    assert!(runner.clear_authentication("../fixture").is_err());
    runner.clear_authentication("fixture").unwrap();
    assert!(!auth.exists());
    let expected_job=std::process::Command::new(node()).args(["-e","const {resolve}=require('path');process.stdout.write('Global\\\\Ivy.Collector.'+require('crypto').createHash('sha256').update(resolve(process.argv[1],'fixture').toLowerCase()).digest('hex'))",&root.0.join("work").to_string_lossy()]).output().unwrap();
    assert!(expected_job.status.success());
    assert_eq!(
        runner.job_name("fixture").unwrap(),
        String::from_utf8(expected_job.stdout).unwrap()
    );
    assert!(
        !runner
            .stopped("fixture".into(), "Global\\Ivy.Collector.wrong".into())
            .await
            .unwrap()
    );
    #[cfg(windows)]
    assert!(
        runner
            .stopped("fixture".into(), runner.job_name("fixture").unwrap())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn npm_preparation_uses_exact_flags_ordered_hash_cache_and_no_stale_modules() {
    let root = Root::new();
    let cli = root.0.join("npm-fixture.mjs");
    std::fs::write(&cli,"import{existsSync,appendFileSync,mkdirSync,writeFileSync}from'node:fs';if(process.platform==='win32'&&!existsSync('../../registered'))process.exit(4);appendFileSync('install.jsonl',JSON.stringify(process.argv.slice(2))+'\\n');mkdirSync('node_modules',{recursive:true});writeFileSync('node_modules/prepared','yes');").unwrap();
    let runner = runner(
        &root,
        IvyJson::object([("npmCliPath", cli.to_string_lossy().as_ref().into())]),
        None,
    )
    .await;
    let mut value = task("export default async()=>({data:1})");
    value.insert(
        "dependencies",
        parse(r#"{"z":"1.0.0","a":"2.0.0"}"#, 100).unwrap(),
    );
    run(&runner, value.clone(), &root).await.unwrap();
    run(&runner, value.clone(), &root).await.unwrap();
    let cwd = root.0.join("work/fixture");
    let installs = std::fs::read_to_string(cwd.join("install.jsonl")).unwrap();
    assert_eq!(installs.lines().count(), 1);
    assert_eq!(parse(installs.trim(),1000).unwrap(),parse(r#"["install","--omit=dev","--ignore-scripts","--no-audit","--no-fund","--loglevel=error"]"#,1000).unwrap());
    let package = parse(
        &std::fs::read_to_string(cwd.join("package.json")).unwrap(),
        1000,
    )
    .unwrap();
    assert_eq!(package.get("dependencies"), value.get("dependencies"));
    let expected=std::process::Command::new(node()).args(["-e","process.stdout.write(require('crypto').createHash('sha256').update(process.argv[1]).digest('hex'))",&stringify(value.get("dependencies").unwrap(),1000).unwrap()]).output().unwrap();
    assert_eq!(
        std::fs::read(cwd.join("dependencies.sha256")).unwrap(),
        expected.stdout
    );
    value.insert(
        "dependencies",
        parse(r#"{"a":"2.0.0","z":"1.0.0"}"#, 100).unwrap(),
    );
    run(&runner, value.clone(), &root).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(cwd.join("install.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    value.insert(
        "dependencies",
        IvyJson::object(Vec::<(&str, IvyJson)>::new()),
    );
    run(&runner, value.clone(), &root).await.unwrap();
    assert!(!cwd.join("node_modules").exists());
    value.insert("dependencies", parse(r#"{"a":"2.0.0"}"#, 100).unwrap());
    std::fs::remove_file(cli).unwrap();
    assert_eq!(
        run(&runner, value, &root).await.unwrap_err().code,
        "npm_unavailable"
    );
}

#[tokio::test]
async fn cancellation_deadline_and_failed_registration_never_leave_a_live_worker() {
    let root = Root::new();
    let runner =
        Arc::new(runner(&root, IvyJson::object(Vec::<(&str, IvyJson)>::new()), None).await);
    let script = "import{writeFileSync}from'node:fs';import{spawn}from'node:child_process';export default async()=>{spawn(process.execPath,['-e',\"setInterval(()=>require('fs').writeFileSync('pulse',Date.now()+''),20)\"],{stdio:'ignore'});writeFileSync('started','yes');await new Promise(r=>setTimeout(r,60000));return {data:1}}";
    #[cfg(windows)]
    {
        let reject: RegisterJob =
            Arc::new(|_| Box::pin(async { Err(IvyError::new("registration_failed", "fixture")) }));
        assert_eq!(
            runner
                .run(
                    task(script),
                    IvyJson::Null,
                    IvyJson::Null,
                    CancellationToken::new(),
                    reject
                )
                .await
                .unwrap_err()
                .code,
            "registration_failed"
        );
        assert!(!root.0.join("work/fixture").exists());
    }
    let signal = CancellationToken::new();
    let pending = runner.run(
        task(script),
        IvyJson::Null,
        IvyJson::Null,
        signal.clone(),
        register(&root),
    );
    let work = tokio::spawn(pending);
    let pulse = root.0.join("work/fixture/pulse");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !pulse.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    signal.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), work)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    let before = std::fs::read(&pulse).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(before, std::fs::read(&pulse).unwrap());
    let mut value = task(script);
    value.insert("timeoutSeconds", IvyJson::Number(0.2));
    let error = run(&runner, value, &root).await.unwrap_err();
    assert_eq!(error.code, "deadline_exceeded");
    let before = std::fs::read(&pulse).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(before, std::fs::read(&pulse).unwrap());
    #[cfg(windows)]
    assert!(
        runner
            .stopped("fixture".into(), runner.job_name("fixture").unwrap())
            .await
            .unwrap()
    );
}

struct Bridge(Arc<AtomicUsize>);
impl McpBridge for Bridge {
    fn context(&self) -> IvyJson {
        IvyJson::object([
            ("url", "http://127.0.0.1:1/call".into()),
            ("token", "fixture".into()),
        ])
    }
    fn close(&self) -> ServiceFuture<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}
struct Factory(Arc<AtomicUsize>);
impl McpBridgeFactory for Factory {
    fn open(&self, _: IvyJson, _: CancellationToken) -> ServiceFuture<Arc<dyn McpBridge>> {
        let bridge: Arc<dyn McpBridge> = Arc::new(Bridge(self.0.clone()));
        Box::pin(async move { Ok(bridge) })
    }
}
#[tokio::test]
async fn mcp_bridge_is_closed_after_both_success_and_script_failure() {
    let root = Root::new();
    let closed = Arc::new(AtomicUsize::new(0));
    let runner = runner(
        &root,
        IvyJson::object(Vec::<(&str, IvyJson)>::new()),
        Some(Arc::new(Factory(closed.clone()))),
    )
    .await;
    for (script, ok) in [
        ("export default async c=>({data:typeof c.mcp.call})", true),
        ("export default async()=>{throw Error('fixture')}", false),
    ] {
        let mut value = task(script);
        value.insert("mcp", IvyJson::Null);
        assert_eq!(run(&runner, value, &root).await.is_ok(), ok);
    }
    assert_eq!(closed.load(Ordering::SeqCst), 2);
}
