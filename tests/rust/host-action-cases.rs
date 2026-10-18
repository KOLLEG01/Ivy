use super::{ServiceFixture, host_configuration, assert_json_eq};
use ivy_contracts::{IvyJson, IvyResult, JsString, json};
use ivy_host_runtime::{actions::{self, Action, LocalAction}, config, journal::HostJournal, node_path, source};
use std::{fs, io::Write, path::{Path, PathBuf}, process::{Command, Stdio}, sync::Mutex, time::Duration};

static CORPUS: Mutex<()> = Mutex::new(());
fn text(path: &Path) -> IvyJson { IvyJson::String(node_path::from_native(path)) }
fn path(value: &IvyJson) -> PathBuf { node_path::filesystem_path(value.as_js_string().unwrap()) }
fn oracle(input: IvyJson) -> IvyJson {
    let mut child=Command::new("node").args(["--experimental-transform-types","--disable-warning=ExperimentalWarning","tests/fixtures/host-actions-oracle.mjs"])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(json::stringify(&input,usize::MAX).unwrap().as_bytes()).unwrap();
    let output=child.wait_with_output().unwrap();assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    json::parse(std::str::from_utf8(&output.stdout).unwrap(),usize::MAX).unwrap()
}
fn compare(input: IvyJson, result: IvyResult<IvyJson>) {
    let actual=match result {Ok(value)=>IvyJson::object([("ok",true.into()),("value",value)]),Err(error)=>IvyJson::object([("ok",false.into()),("error",error.to_wire())])};
    let expected=oracle(input.clone());
    if let Some(file)=std::env::var_os("IVY_HOST_ACTION_CORPUS") {
        let _guard=CORPUS.lock().unwrap();let mut file=fs::OpenOptions::new().create(true).append(true).open(file).unwrap();
        writeln!(file,"{}",json::stringify(&IvyJson::object([("input",input), ("actual",actual.clone()), ("expected",expected.clone())]),usize::MAX).unwrap()).unwrap();
    }
    assert_json_eq(&actual,&expected);
}
struct Fixture { root: ServiceFixture, config: IvyJson, git: PathBuf, source: PathBuf }
impl Fixture {
    async fn new() -> Self {
        let root=ServiceFixture::new();let source=root.0.join("checkout");fs::create_dir_all(source.join("packages")).unwrap();
        let git=Command::new(if cfg!(windows){"where.exe"}else{"which"}).arg("git").output().unwrap();
        assert!(git.status.success());let git=PathBuf::from(String::from_utf8(git.stdout).unwrap().lines().next().unwrap());
        let node=Command::new("node").args(["-p","process.execPath"]).output().unwrap();assert!(node.status.success());
        let node=String::from_utf8(node.stdout).unwrap().trim().to_owned();
        let mut config=host_configuration(&root);config.insert("executables",IvyJson::object([("node",node.into()),("git",text(&git))]));
        let configuration=root.0.join("host.json");config.insert("configPath",text(&configuration));
        config::atomic_json(configuration,config.clone()).await.unwrap();
        #[cfg(windows)] {fs::create_dir_all(root.0.join("dist/native")).unwrap();fs::copy(env!("CARGO_BIN_EXE_ivy-job"),root.0.join("dist/native/ivy-job.exe")).unwrap();}
        let fixture=Self{root,config,git,source};
        fixture.write("package.json",b"{\"name\":\"fixture\",\"version\":\"1.0.0\",\"private\":true}\n");
        fixture.write("package-lock.json",b"{\"name\":\"fixture\",\"lockfileVersion\":3,\"packages\":{}}\n");
        fixture.write(".gitignore",b"node_modules/\n.env\nconfig.json\ngenerated/\n");
        fixture.write("packages/kept.ts",b"before\n");fixture.write("packages/deleted.ts",b"removed\n");
        fixture.git(&["init","--quiet"]);fixture.git(&["add","."]);
        fixture.write("packages/kept.ts",b"changed\r\n");fs::remove_file(fixture.source.join("packages/deleted.ts")).unwrap();
        fixture
    }
    fn write(&self, name: &str, bytes: &[u8]) {let p=self.source.join(name);fs::create_dir_all(p.parent().unwrap()).unwrap();fs::write(p,bytes).unwrap();}
    fn git(&self,args: &[&str]) {
        let mut command=Command::new(&self.git);command.args(args).current_dir(&self.source).stdin(Stdio::null());
        #[cfg(windows)] {use std::os::windows::process::CommandExt;command.creation_flags(0x08000000);}
        let output=command.output().unwrap();assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    }
    async fn capture(&self) -> IvyResult<IvyJson> {source::capture_source(node_path::from_native(&self.source),self.config.clone(),self.root.0.clone()).await}
    fn input(&self) -> IvyJson {IvyJson::object([("kind","capture".into()),("source",text(&self.source)),("config",self.config.clone())])}
    async fn journal(&self) -> HostJournal {HostJournal::open(self.config.clone()).await.unwrap()}
    fn action(&self,action:Action,operation:&str) -> LocalAction {LocalAction {action,instance_id:"fixture".into(),operation_id:operation.into(),candidate_id:None,source:None,target_build:None}}
    fn no_incoming(&self) {
        let root=path(self.config.required("stagingRoot").unwrap());
        if root.exists(){assert!(!fs::read_dir(root).unwrap().any(|entry| entry.unwrap().file_name().to_string_lossy().starts_with("incoming-")));}
    }
}
fn view(snapshot: &IvyJson) -> IvyJson {
    fn walk(root:&Path,directory:&Path,files:&mut Vec<IvyJson>) {
        let mut entries=fs::read_dir(directory).unwrap().collect::<Result<Vec<_>,_>>().unwrap();entries.sort_by_key(|e|e.file_name());
        for entry in entries {let file=entry.path();let metadata=fs::symlink_metadata(&file).unwrap();
            if metadata.is_dir(){walk(root,&file,files)}else{
                let mut value=IvyJson::object([("path",file.strip_prefix(root).unwrap().to_string_lossy().replace('\\',"/").into()),("hex",fs::read(file).unwrap().iter().map(|b|format!("{b:02x}")).collect::<String>().into())]);
                #[cfg(unix)] {use std::os::unix::fs::MetadataExt;value.insert("mode",IvyJson::Number((metadata.mode()&0o777) as f64));}
                #[cfg(not(unix))] let _=&mut value;
                files.push(value);
            }
        }
    }
    let mut files=Vec::new();let root=path(snapshot.required("sourceRoot").unwrap());walk(&root,&root,&mut files);
    IvyJson::object([("files",IvyJson::Array(files)),("originalRoot",snapshot.required("originalRoot").unwrap().clone())])
}
async fn compare_capture(f: &Fixture) -> IvyJson {
    let snapshot=f.capture().await.unwrap();let actual=view(&snapshot);
    compare(f.input(),Ok(actual));f.no_incoming();snapshot
}

#[tokio::test]
async fn source_capture_preserves_fixed_uncommitted_bytes_and_original_allowlist() {
    let f=Fixture::new().await;
    for (name,bytes) in [
        ("packages/new.ts",b"new\n".as_slice()),("packages/binary.bin",&[0,13,10,255,128,1]),
        (".gitattributes",b"* text=auto eol=lf\n"),("LICENSE",b"fixture license\n"),
        ("tsconfig.web.json",b"{\"compilerOptions\":{\"strict\":true}}\n"),
        ("services/example/src/runtime/module.ts",b"runtime source\n"),("services/companion/extension/service-worker.mjs",b"extension source\n"),
        ("instructions/skills/example/SKILL.md",b"# Instruction\n"),("packages/example/rust/src/lib.rs",b"pub fn fixture() {}\n"),
        (".agents/private.json",b"excluded"),("config.json",b"excluded"),("packages/nested/config.json",b"excluded"),
        ("packages/.env",b"excluded"),("packages/.env.local",b"excluded"),("packages/dist/file",b"excluded"),
        ("packages/node_modules/file",b"excluded"),("packages/.local/file",b"excluded"),("services/example/generated/file",b"ignored"),
    ] {f.write(name,bytes);}
    #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;fs::set_permissions(f.source.join("packages/new.ts"),fs::Permissions::from_mode(0o755)).unwrap();}
    let snapshot=compare_capture(&f).await;
    let captured=view(&snapshot);f.write("packages/new.ts",b"later");f.write("package-lock.json",b"changed lock");
    source::verify_snapshot(snapshot.clone(),path(f.config.required("stagingRoot").unwrap())).await.unwrap();assert_json_eq(&view(&snapshot),&captured);
    assert_eq!(fs::read(path(snapshot.required("sourceRoot").unwrap()).join("packages/new.ts")).unwrap(),b"new\n");
    let manifest=config::json_file(path(snapshot.required("manifestPath").unwrap()),8*1024*1024).await.unwrap();assert_json_eq(manifest.required("snapshot").unwrap(),&snapshot);
}

#[tokio::test]
async fn source_capture_rejects_preconditions_links_and_limits_before_publication() {
    for mode in ["missing-git","relative","no-checkout","no-lock","file-link","parent-link","large-file"] {
        let mut f=Fixture::new().await;let mut original=node_path::from_native(&f.source);
        match mode {
            "missing-git"=>f.config.insert("executables",IvyJson::object([("node","unused".into())])),
            "relative"=>original="relative".into(),
            "no-checkout"=>fs::remove_dir_all(f.source.join(".git")).unwrap(),
            "no-lock"=>{fs::remove_file(f.source.join("package-lock.json")).unwrap();f.git(&["rm","--cached","package-lock.json"]);},
            "file-link"=>{let target=f.root.0.join("private-file");fs::write(&target,b"private").unwrap();
                #[cfg(windows)] std::os::windows::fs::symlink_file(target,f.source.join("packages/link")).unwrap();
                #[cfg(unix)] std::os::unix::fs::symlink(target,f.source.join("packages/link")).unwrap();},
            "parent-link"=>{let target=f.root.0.join("private-directory");fs::create_dir(&target).unwrap();fs::write(target.join("file"),b"private").unwrap();
                // Track a path, then replace its containing directory. ls-files
                // retains it even though Git will not enumerate the new link.
                f.write("packages/linked/file",b"original");f.git(&["add","packages/linked/file"]);fs::remove_dir_all(f.source.join("packages/linked")).unwrap();
                #[cfg(windows)] std::os::windows::fs::symlink_dir(target,f.source.join("packages/linked")).unwrap();
                #[cfg(unix)] std::os::unix::fs::symlink(target,f.source.join("packages/linked")).unwrap();},
            "large-file"=>fs::File::create(f.source.join("packages/large")).unwrap().set_len(64*1024*1024+1).unwrap(),
            _=>unreachable!(),
        }
        let result=source::capture_source(original.clone(),f.config.clone(),f.root.0.clone()).await.map(|s|view(&s));
        assert!(result.is_err(),"{mode}");let mut input=f.input();input.insert("source",IvyJson::String(original));compare(input,result);f.no_incoming();
    }
}

#[tokio::test]
async fn source_capture_rejects_private_roots_and_configuration_and_cleans_failed_promotion() {
    for mode in ["runtime","artifacts","staging","service","projects","configuration","promotion"] {
        let mut f=Fixture::new().await;
        match mode {
            "runtime"=>f.config.insert("runtimeRoot",text(&f.source.join("state"))),
            "artifacts"=>f.config.insert("artifactRoot",text(&f.source.join("artifacts"))),
            "staging"=>f.config.insert("stagingRoot",text(&f.source.join("staging"))),
            "service"|"projects"=>{let mut instances=f.config.required("instances").unwrap().as_array().unwrap().to_vec();
                if mode=="projects" {instances[0].insert("componentId","agent-manager".into());instances[0].insert("settings",IvyJson::object([("internalProjectRoot",text(&f.source.join("internal")))]));}
                else {instances[0].insert("paths",IvyJson::object([("data",text(&f.source.join("data"))),("work",text(&f.root.0.join("work"))),("logs",text(&f.root.0.join("logs")))]));}
                f.config.insert("instances",IvyJson::Array(instances));},
            "configuration"=>f.config.insert("configPath",text(&f.source.join("host.json"))),
            "promotion"=>{let staging=path(f.config.required("stagingRoot").unwrap());fs::create_dir_all(&staging).unwrap();fs::write(staging.join("sources"),b"keep this file").unwrap();},
            _=>unreachable!(),
        }
        compare(f.input(),f.capture().await.map(|s|view(&s)));f.no_incoming();
        assert_eq!(fs::read(f.source.join("packages/kept.ts")).unwrap(),b"changed\r\n");
    }
}

#[tokio::test]
async fn source_git_capture_keeps_sync_stdin_raw_buffer_limits_and_path_admission() {
    for mode in ["stdin-eof","combined-buffer","invalid-utf8","duplicate-paths","traversal","path-count","nonzero"] {
        let mut f=Fixture::new().await;
        let node=f.config.required("executables").unwrap().required("node").unwrap().clone();
        f.config.insert("executables",IvyJson::object([("node",node.clone()),("git",node)]));
        let paths="package.json\\0package-lock.json\\0packages/kept.ts\\0";
        let script=match mode {
            "stdin-eof"=>format!("process.stdin.resume();process.stdin.on('end',()=>process.stdout.write('{paths}'));"),
            "combined-buffer"=>"process.stdout.write(Buffer.alloc(40*1024*1024,120));process.stderr.write(Buffer.alloc(40*1024*1024,121));".into(),
            "invalid-utf8"=>{f.write("packages/�.ts",b"replacement-name bytes");format!("process.stdout.write('{paths}');process.stdout.write(Buffer.from([112,97,99,107,97,103,101,115,47,255,46,116,115,0]));")},
            "duplicate-paths"=>format!("process.stdout.write('{paths}{paths}packages/.env\\0private/ignored\\0');"),
            "traversal"=>{fs::write(f.root.0.join("private-file"),b"private").unwrap();format!("process.stdout.write('{paths}packages/../../private-file\\0');")},
            "path-count"=>format!("process.stdout.write('{paths}'+Array.from({{length:20001}},(_,i)=>'packages/file-'+i+'\\0').join(''));"),
            "nonzero"=>format!("process.stdout.write('{paths}');process.exitCode=7;"),
            _=>unreachable!(),
        };
        fs::write(f.source.join("ls-files"),script).unwrap();
        let result=f.capture().await.map(|s|view(&s));
        assert_eq!(result.is_ok(),matches!(mode,"stdin-eof"|"invalid-utf8"|"duplicate-paths"),"{mode}: {result:?}");
        compare(f.input(),result);f.no_incoming();
    }
}

#[tokio::test]
async fn source_total_limit_counts_hard_link_names_before_copying_payloads() {
    let f=Fixture::new().await;let file=f.source.join("packages/payload-0");
    fs::File::create(&file).unwrap().set_len(64*1024*1024).unwrap();
    for i in 1..17 {fs::hard_link(&file,f.source.join(format!("packages/payload-{i}"))).unwrap();}
    let result=f.capture().await;assert_eq!(result.as_ref().unwrap_err().message,"Source copy exceeds its byte limit.");
    compare(f.input(),result.map(|s|view(&s)));f.no_incoming();
}

#[cfg(windows)]
#[tokio::test]
async fn source_publication_uses_node_fs_string_conversion_and_retains_json_utf16() {
    let mut f=Fixture::new().await;
    let mut staging=node_path::from_native(&f.root.0.join("staging-"));staging.0.push(0xd800);
    f.config.insert("stagingRoot",IvyJson::String(staging.clone()));
    let snapshot=compare_capture(&f).await;
    assert!(snapshot.required("sourceRoot").unwrap().as_js_string().unwrap().0.contains(&0xd800));
    assert!(node_path::filesystem_path(&staging).is_dir());
}

#[cfg(windows)]
#[tokio::test]
async fn configured_git_js_string_is_encoded_before_native_executable_resolution() {
    use std::os::windows::ffi::OsStringExt;
    let mut f=Fixture::new().await;
    let node=path(f.config.required("executables").unwrap().required("node").unwrap());
    let mut name="git-".encode_utf16().collect::<Vec<_>>();name.push(0xd800);name.extend(".exe".encode_utf16());
    let executable=f.root.0.join(std::ffi::OsString::from_wide(&name));fs::copy(&node,&executable).unwrap();
    f.config.insert("executables",IvyJson::object([("node",text(&node)),("git",text(&executable))]));
    fs::write(f.source.join("ls-files"),"process.stdout.write('package.json\\0package-lock.json\\0packages/kept.ts\\0');").unwrap();
    let result=f.capture().await;assert_eq!(result.as_ref().unwrap_err().message,"Source must be an accessible Git checkout.");
    compare(f.input(),result.map(|s|view(&s)));f.no_incoming();
}

fn hash() -> IvyJson {format!("sha256:{}","a".repeat(64)).into()}
async fn seed(journal:&HostJournal) {
    journal.save_candidate(super::journal_cases::candidate(),super::journal_cases::manifest()).await.unwrap();
    journal.set_installed(IvyJson::object([("instanceId","fixture".into()),("candidateId",hash()),("buildId",hash()),("enabled",true.into()),("installedAt","2026-01-01T00:00:00.000Z".into())])).await.unwrap();
}
fn cli_input(f:&Fixture,action:Action,operation:&str,arguments:&[(&str,JsString)]) -> IvyJson {
    let mut args=vec![IvyJson::from(action.as_str()),"--config".into(),f.config.required("configPath").unwrap().clone(),"--instance".into(),"fixture".into(),"--operation-id".into(),operation.into(),"--json".into()];
    for (key,value) in arguments {args.extend([(*key).into(),IvyJson::String(value.clone())]);}
    IvyJson::object([("kind","cli".into()),("args",IvyJson::Array(args))])
}
fn compare_replayed_cli(input:IvyJson,entry:&IvyJson) {
    let output=oracle(input);assert_eq!(output.get("ok"),Some(&true.into()),"{output:?}");
    let result=output.required("value").unwrap();assert_eq!(result.required("output").unwrap().get("deploymentId"),entry.required("record").unwrap().get("deploymentId"));
    assert_json_eq(result.required("output").unwrap().required("data").unwrap(),entry);
}

#[tokio::test]
async fn bootstrap_owner_guard_precedes_source_capture_and_matches_original_cli() {
    for component in ["host-executor", "service-manager"] {
        if component=="service-manager" && !cfg!(windows) {continue;}
        let mut f=Fixture::new().await;
        let mut instances=f.config.required("instances").unwrap().as_array().unwrap().to_vec();instances[0].insert("componentId",component.into());f.config.insert("instances",IvyJson::Array(instances));
        config::atomic_json(path(f.config.required("configPath").unwrap()),f.config.clone()).await.unwrap();
        let journal=f.journal().await;seed(&journal).await;
        let mut action=f.action(Action::Deploy,"bootstrap-guard");action.source=Some(node_path::from_native(&f.root.0.join("absent")));
        let error=actions::accept_local_action(journal.clone(),action,f.root.0.clone()).await.unwrap_err();assert_eq!(error.code,"restart_required");
        assert!(!path(f.config.required("stagingRoot").unwrap()).exists());
        let result=oracle(cli_input(&f,Action::Deploy,"bootstrap-guard",&[("--source",node_path::from_native(&f.root.0.join("absent")))]));
        let output=result.required("value").unwrap().required("output").unwrap();assert_eq!(output.get("code"),Some(&error.code.into()));assert_eq!(output.required("data").unwrap().get("message"),Some(&error.message.into()));
        assert!(journal.operation("bootstrap-guard").await.unwrap().is_none());f.no_incoming();journal.close().await.unwrap();
    }
}

#[tokio::test]
async fn local_actions_share_all_five_mutations_and_terminal_replay_with_original_cli() {
    let f=Fixture::new().await;let journal=f.journal().await;seed(&journal).await;
    for action in [Action::Deploy,Action::Rollback,Action::Restart,Action::Enable,Action::Disable] {
        let op=format!("action-{}",action.as_str());let mut request=f.action(action,&op);let mut options=Vec::new();
        if action==Action::Deploy {request.candidate_id=Some(hash().as_js_string().unwrap().clone());options.push(("--candidate",request.candidate_id.clone().unwrap()));}
        if action==Action::Rollback {request.target_build=Some(hash().as_js_string().unwrap().clone());options.push(("--to",request.target_build.clone().unwrap()));}
        let entry=actions::accept_local_action(journal.clone(),request,f.root.0.clone()).await.unwrap();
        assert_eq!(entry.required("record").unwrap().get("phase"),Some(&"prepared".into()));
        assert_eq!(entry.required("activation").unwrap().get("targetEnabled"),Some(&(action!=Action::Disable).into()));
        compare_replayed_cli(cli_input(&f,action,&op,&options),&entry);
        let id=entry.required("record").unwrap().required("deploymentId").unwrap().as_str().unwrap();
        journal.advance(&id,"prepared","failed",IvyJson::object([("errorCode","fixture_failed".into())])).await.unwrap();
        let terminal=journal.get(&id).await.unwrap();compare_replayed_cli(cli_input(&f,action,&op,&options),&terminal);
    }
    journal.close().await.unwrap();
}

#[tokio::test]
async fn local_source_replay_after_checkout_loss_keeps_the_exact_durable_entry() {
    let f=Fixture::new().await;let journal=f.journal().await;
    let mut action=f.action(Action::Deploy,"source-fixed");action.source=Some(node_path::from_native(&f.source));
    let accepted=actions::accept_local_action(journal.clone(),action,f.root.0.clone()).await.unwrap();let before=view(accepted.required("sourceSnapshot").unwrap());
    fs::remove_dir_all(&f.source).unwrap();journal.close().await.unwrap();let journal=f.journal().await;
    let mut action=f.action(Action::Deploy,"source-fixed");action.source=Some(node_path::from_native(&f.source));
    assert_json_eq(&actions::accept_local_action(journal.clone(),action,f.root.0.clone()).await.unwrap(),&accepted);
    compare_replayed_cli(cli_input(&f,Action::Deploy,"source-fixed",&[("--source",node_path::from_native(&f.source))]),&accepted);
    let mut changed=f.action(Action::Deploy,"source-fixed");changed.source=Some(node_path::from_native(&f.root.0.join("other")));
    assert_eq!(actions::accept_local_action(journal.clone(),changed,f.root.0.clone()).await.unwrap_err().code,"mutation_conflict");
    assert_json_eq(&view(accepted.required("sourceSnapshot").unwrap()),&before);f.no_incoming();journal.close().await.unwrap();
}

#[tokio::test]
async fn dropped_action_waiter_retains_capture_until_the_original_journal_write_finishes() {
    let f=Fixture::new().await;let journal=f.journal().await;
    let lock=ivy_store::rusqlite::Connection::open(path(f.config.required("runtimeRoot").unwrap()).join("deployments.sqlite")).unwrap();lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut action=f.action(Action::Deploy,"dropped-source");action.source=Some(node_path::from_native(&f.source));
    let owned=journal.clone();let bootstrap=f.root.0.clone();let waiter=tokio::spawn(async move {actions::accept_local_action(owned,action,bootstrap).await});
    let sources=path(f.config.required("stagingRoot").unwrap()).join("sources");
    tokio::time::timeout(Duration::from_secs(8),async {while !sources.is_dir() || fs::read_dir(&sources).unwrap().count()!=1 {tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
    waiter.abort();let _=waiter.await;lock.execute_batch("COMMIT").unwrap();
    let entry=tokio::time::timeout(Duration::from_secs(8),async {loop {if let Some(entry)=journal.operation("dropped-source").await.unwrap(){break entry;}tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
    assert_eq!(entry.required("record").unwrap().get("phase"),Some(&"preparing".into()));assert_eq!(fs::read_dir(sources).unwrap().count(),1);
    compare_replayed_cli(cli_input(&f,Action::Deploy,"dropped-source",&[("--source",node_path::from_native(&f.source))]),&entry);f.no_incoming();journal.close().await.unwrap();
}

#[test]
fn manager_action_admission_binds_original_operation_before_any_io() {
    let args=IvyJson::object([("instanceId","fixture".into()),("operationId","local-operation".into()),("source","relative-checkout".into())]);
    assert_eq!(LocalAction::from_manager(Action::Deploy,&args,Some(&"other-operation".into())).err().unwrap().code,"mutation_conflict");
    let action=LocalAction::from_manager(Action::Deploy,&args,Some(&"local-operation".into())).unwrap();let request=action.request().unwrap();
    assert_eq!(request.required("source").unwrap().as_js_string().unwrap(),&node_path::Context::current().unwrap().resolve(&["relative-checkout".into()]));
    assert_eq!(LocalAction::from_manager(Action::Rollback,&args,None).err().unwrap().message,"This option does not belong to the selected command.");
    let mut values=args;values.insert("source",IvyJson::Null);values.insert("candidateId",IvyJson::Number(7.0));
    let request=LocalAction::from_manager(Action::Deploy,&values,None).unwrap().request().unwrap();assert_eq!(request.get("candidateId"),Some(&"7".into()));assert!(request.get("source").is_none());
}
