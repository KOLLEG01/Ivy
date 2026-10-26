#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use ivy_agent_manager::{
    environment::select_environment_document,
    instructions::{ComposedInstructions, compose_instructions},
    instructions_files::{InstructionsSource, regular_bytes},
    instructions_writer::InstructionsWriter,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::{digest, hash_json},
    json::{canonical, parse},
};
use std::{
    fs,
    future::Future,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    pin::Pin,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::Semaphore;

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("ivy-instructions-files-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn string(value: &IvyJson) -> String {
    value.as_js_string().unwrap().to_utf8().unwrap()
}
fn field<'a>(value: &'a IvyJson, name: &str) -> &'a IvyJson {
    value.get(name).unwrap()
}
fn option(value: &IvyJson) -> Option<&IvyJson> {
    (value != &IvyJson::Null).then_some(value)
}
fn oracle() -> IvyJson {
    let output = reference_data::read(include_bytes!("../fixtures/agent-instructions-files-cases.json.gz"));
    parse(
        std::str::from_utf8(&output).unwrap(),
        4 * 1024 * 1024,
    )
    .unwrap()
}
fn unhex(value: &IvyJson) -> Vec<u8> {
    let text = string(value);
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
fn install(home: &Path, files: &IvyJson) {
    for (key, entry) in files.as_object().unwrap() {
        let path = home.join(key.to_utf8().unwrap());
        match string(field(entry, "kind")).as_str() {
            "directory" => fs::create_dir_all(path).unwrap(),
            "repeat" => write(
                &path,
                vec![
                    field(entry, "byte").as_f64().unwrap() as u8;
                    field(entry, "size").as_f64().unwrap() as usize
                ],
            ),
            "file" => write(&path, unhex(field(entry, "hex"))),
            _ => panic!("Unexpected input entry"),
        }
    }
}
fn snapshot(home: &Path) -> IvyJson {
    fn walk(home: &Path, path: &Path, result: &mut Vec<(JsString, IvyJson)>) {
        let mut entries = fs::read_dir(path)
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("executor-lock.sqlite")
            {
                continue;
            }
            let path = entry.path();
            let key = path
                .strip_prefix(home)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let info = fs::symlink_metadata(&path).unwrap();
            let value = if info.file_type().is_symlink() {
                IvyJson::object([("kind", "link".into())])
            } else if info.is_dir() {
                IvyJson::object([("kind", "directory".into())])
            } else {
                let bytes = fs::read(&path).unwrap();
                IvyJson::object([
                    ("kind", "file".into()),
                    ("size", IvyJson::Number(bytes.len() as f64)),
                    ("hash", digest(bytes).into()),
                ])
            };
            result.push((key.into(), value));
            if info.is_dir() && !info.file_type().is_symlink() {
                walk(home, &path, result);
            }
        }
    }
    let mut entries = Vec::new();
    walk(home, home, &mut entries);
    IvyJson::Object(entries)
}
fn status(writer: &InstructionsWriter, home: &Path) -> IvyJson {
    let s = writer.status();
    assert_eq!(s.observed_at.len(), 24);
    assert!(s.observed_at.ends_with('Z'));
    let canonical_home = fs::canonicalize(home).unwrap();
    let relative = s
        .home
        .strip_prefix(home)
        .or_else(|_| s.home.strip_prefix(&canonical_home))
        .unwrap_or(&s.home)
        .to_string_lossy()
        .replace('\\', "/");
    IvyJson::object([
        ("state", s.state.into()),
        (
            "home",
            if relative.is_empty() {
                ".".into()
            } else {
                relative.into()
            },
        ),
        (
            "desiredVersion",
            s.desired_version
                .map(IvyJson::from)
                .unwrap_or(IvyJson::Null),
        ),
        ("appliedVersion", s.applied_version),
        ("code", s.code.map(IvyJson::from).unwrap_or(IvyJson::Null)),
        ("bytes", IvyJson::Number(s.bytes as f64)),
    ])
}
fn equal(actual: &IvyJson, expected: &IvyJson, label: &str) {
    assert_eq!(
        canonical(actual, usize::MAX).unwrap(),
        canonical(expected, usize::MAX).unwrap(),
        "{label}"
    );
}
struct Source {
    sequence: Vec<IvyJson>,
    guidance: JsString,
    reads: AtomicUsize,
}
impl Source {
    fn new(sequence: &IvyJson, guidance: &IvyJson) -> Self {
        Self {
            sequence: sequence.as_array().unwrap().to_vec(),
            guidance: guidance.as_js_string().unwrap().clone(),
            reads: AtomicUsize::new(0),
        }
    }
}
impl InstructionsSource for Source {
    fn desired<'a>(
        &'a self,
        home: &'a Path,
    ) -> Pin<Box<dyn Future<Output = IvyResult<ComposedInstructions>> + Send + 'a>> {
        Box::pin(async move {
            let index = self
                .reads
                .fetch_add(1, Ordering::SeqCst)
                .min(self.sequence.len() - 1);
            let source = &self.sequence[index];
            if let Some(error) = source.get("error") {
                return Err(IvyError::new(string(error), "Fixture source unavailable."));
            }
            let defaults = field(source, "defaults");
            let global = option(field(source, "global"));
            let host = option(field(source, "host"));
            let selected = select_environment_document(defaults, global, host)?.document;
            let local = if selected.get("enabled") == Some(&IvyJson::Bool(true))
                && selected.get("includeLocal") == Some(&IvyJson::Bool(true))
            {
                regular_bytes(&home.join("AGENTS.md"))?
            } else {
                None
            };
            compose_instructions(
                global,
                host,
                local.as_deref(),
                Some(defaults),
                &self.guidance,
            )
        })
    }
}
fn writer(home: PathBuf) -> InstructionsWriter {
    InstructionsWriter::new(home, "fixture".into(), "https://fixture.example".into())
}
#[tokio::test]
async fn owned_files_pending_recovery_and_status_match_original_manager() {
    let data = oracle();
    let root = Root::new();
    let rows = field(&data, "rows").as_array().unwrap();
    assert_eq!(rows.len(), 53);
    let mut count = 0;
    for (index, row) in rows.iter().enumerate() {
        let name = string(field(row, "name"));
        let home = root.0.join(index.to_string());
        fs::create_dir(&home).unwrap();
        install(&home, field(row, "files"));
        let writer = writer(home.clone());
        equal(
            &status(&writer, &home),
            field(row, "initial"),
            &format!("{name} initial"),
        );
        for (step, expected) in field(row, "steps")
            .as_array()
            .unwrap()
            .iter()
            .zip(field(row, "outputs").as_array().unwrap())
        {
            if let Some(files) = step.get("files") {
                for (path, value) in files.as_object().unwrap() {
                    write(&home.join(path.to_utf8().unwrap()), unhex(value));
                }
            }
            let source = Arc::new(Source::new(
                field(step, "sources"),
                field(&data, "guidance"),
            ));
            writer.synchronize(source.clone()).wait().await;
            writer.close().wait().await;
            equal(
                &status(&writer, &home),
                field(expected, "status"),
                &format!("{name} status"),
            );
            equal(
                &snapshot(&home),
                field(expected, "files"),
                &format!("{name} files"),
            );
            assert_eq!(
                source.reads.load(Ordering::SeqCst),
                field(expected, "reads").as_f64().unwrap() as usize,
                "{name} reads"
            );
            count += 1;
        }
    }
    assert_eq!(count, 61);
}
#[tokio::test]
async fn real_home_resolution_and_linked_outputs_match_original() {
    let data = oracle();
    let root = Root::new();
    let first = &field(&data, "rows").as_array().unwrap()[0];
    for row in field(&data, "linked").as_array().unwrap() {
        let kind = string(field(row, "kind"));
        let base = root.0.join(&kind);
        let actual = base.join("home");
        let outside = base.join("outside");
        fs::create_dir_all(&actual).unwrap();
        fs::create_dir(&outside).unwrap();
        let link = match kind.as_str() {
            "home" => base.join("link"),
            "root" => actual.join(".ivy-agent-instructions"),
            "target" => actual.join("AGENTS.override.md"),
            "local" => actual.join("AGENTS.md"),
            _ => panic!(),
        };
        let destination = if kind == "home" { &actual } else { &outside };
        let success=Command::new("node").args(["--input-type=module","-e","import{symlinkSync}from'node:fs';symlinkSync(process.argv[1],process.argv[2],process.platform==='win32'?'junction':'dir');"]).arg(destination).arg(&link).status().unwrap();
        assert!(success.success());
        let writer = writer(if kind == "home" { link } else { actual.clone() });
        let source = Arc::new(Source::new(
            field(&field(first, "steps").as_array().unwrap()[0], "sources"),
            field(&data, "guidance"),
        ));
        writer.synchronize(source.clone()).wait().await;
        equal(
            &status(&writer, &actual),
            field(row, "status"),
            &format!("link {kind} status"),
        );
        equal(
            &snapshot(&actual),
            field(row, "files"),
            &format!("link {kind} files"),
        );
        equal(
            &snapshot(&outside),
            field(row, "outside"),
            &format!("link {kind} outside"),
        );
        assert_eq!(
            source.reads.load(Ordering::SeqCst),
            field(row, "reads").as_f64().unwrap() as usize
        );
    }
}
struct Held {
    source: Source,
    started: Semaphore,
    release: Semaphore,
}
impl InstructionsSource for Held {
    fn desired<'a>(
        &'a self,
        home: &'a Path,
    ) -> Pin<Box<dyn Future<Output = IvyResult<ComposedInstructions>> + Send + 'a>> {
        Box::pin(async move {
            if self.source.reads.load(Ordering::SeqCst) == 0 {
                self.started.add_permits(1);
                self.release.acquire().await.unwrap().forget();
            }
            self.source.desired(home).await
        })
    }
}
#[tokio::test]
async fn coalesced_writer_owns_work_after_waiters_are_dropped() {
    let data = oracle();
    let root = Root::new();
    let writer = writer(root.0.clone());
    let sequence = field(
        &field(&field(&data, "rows").as_array().unwrap()[0], "steps")
            .as_array()
            .unwrap()[0],
        "sources",
    );
    let held = Arc::new(Held {
        source: Source::new(sequence, field(&data, "guidance")),
        started: Semaphore::new(0),
        release: Semaphore::new(0),
    });
    let first = writer.synchronize(held.clone());
    held.started.acquire().await.unwrap().forget();
    let unused = Arc::new(Source::new(sequence, field(&data, "guidance")));
    let second = writer.synchronize(unused.clone());
    let close = writer.close();
    assert_eq!(writer.status().state, "pending");
    drop(first);
    drop(second);
    held.release.add_permits(1);
    tokio::time::timeout(std::time::Duration::from_secs(10), close.wait())
        .await
        .unwrap();
    assert_eq!(held.source.reads.load(Ordering::SeqCst), 2);
    assert_eq!(unused.reads.load(Ordering::SeqCst), 0);
    assert_eq!(writer.status().state, "applied");
    let bytes = fs::read(root.0.join("AGENTS.override.md")).unwrap();
    assert_eq!(writer.status().bytes, bytes.len());
    drop(writer.synchronize(unused.clone()));
    writer.close().wait().await;
    assert_eq!(unused.reads.load(Ordering::SeqCst), 1);
    let persisted = parse(
        &fs::read_to_string(root.0.join(".ivy-agent-instructions/state.json")).unwrap(),
        1024 * 1024,
    )
    .unwrap();
    assert_eq!(persisted.get("pending"), Some(&IvyJson::Null));
    assert_eq!(
        persisted.get("appliedHash"),
        Some(&IvyJson::from(digest(bytes)))
    );
}
#[tokio::test]
async fn original_node_lock_excludes_instruction_writer_then_releases() {
    let data = oracle();
    let root = Root::new();
    let lock_root = root.0.join(".ivy-agent-instructions");
    let mut child=Command::new("node").args(["--input-type=module","-e","import{ExecutorLock}from'./dist/packages/host-runtime/src/journal.js';const lock=new ExecutorLock(process.argv[1]);console.log('held');process.stdin.once('data',()=>{lock.close();process.exit(0);});"]).arg(&lock_root).current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line.trim(), "held");
    let sequence = field(
        &field(&field(&data, "rows").as_array().unwrap()[0], "steps")
            .as_array()
            .unwrap()[0],
        "sources",
    );
    let source = Arc::new(Source::new(sequence, field(&data, "guidance")));
    let writer = writer(root.0.clone());
    writer.synchronize(source.clone()).wait().await;
    child.stdin.take().unwrap().write_all(b"close\n").unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(
        writer.status().code.as_deref(),
        Some("executor_already_running")
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), 0);
    assert!(!root.0.join("AGENTS.override.md").exists());
    writer.synchronize(source.clone()).wait().await;
    assert_eq!(writer.status().state, "applied");
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    let persisted = parse(
        &fs::read_to_string(root.0.join(".ivy-agent-instructions/state.json")).unwrap(),
        1024 * 1024,
    )
    .unwrap();
    assert_eq!(
        field(&persisted, "authority"),
        &IvyJson::from(
            hash_json(&IvyJson::object([
                ("hive", "https://fixture.example".into()),
                ("hostId", "fixture".into())
            ]))
            .unwrap()
        )
    );
}
