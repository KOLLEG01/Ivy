use ivy_contracts::{IvyJson, JsString, json};
use ivy_host_runtime::{
    emitted_runtime::{JavaScriptImports, emitted_runtime_packages},
    node_path,
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};
static CORPUS: Mutex<()> = Mutex::new(());
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ivy-emitted-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        let file = self.0.join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, bytes).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn root() -> PathBuf {
    std::env::var_os("IVY_HOST_SOURCE_ROOT").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        PathBuf::from,
    )
}
fn oracle(path: &Path) -> IvyJson {
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/host-emitted-runtime-oracle.mjs",
        ])
        .current_dir(root())
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
            json::stringify(
                &IvyJson::object([("root", IvyJson::String(node_path::from_native(path)))]),
                usize::MAX,
            )
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
    json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
}
fn append(value: IvyJson) {
    if let Some(path) = std::env::var_os("IVY_HOST_EMITTED_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap(),
            "{}",
            json::stringify(&value, usize::MAX).unwrap()
        )
        .unwrap();
    }
}
async fn compare(path: PathBuf) -> IvyJson {
    let expected = oracle(&path);
    let mut table = Vec::new();
    for row in expected.required("parsed").unwrap().as_array().unwrap() {
        table.push((
            row.required("path")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            row.required("source")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            row.required("imports")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_js_string().unwrap().clone())
                .collect::<Vec<_>>(),
        ));
    }
    let observed = Arc::new(Mutex::new(Vec::new()));
    let calls = observed.clone();
    let records = table.clone();
    let parser: JavaScriptImports = Arc::new(move |path, source| {
        let index = records
            .iter()
            .position(|(file, text, _)| file == path && text == source)
            .expect("parser fixture has no unknown-file/source fallback");
        calls.lock().unwrap().push(path.clone());
        Ok(records[index].2.clone())
    });
    let result = emitted_runtime_packages(path.clone(), parser).await;
    let actual = match result {
        Ok(names) => IvyJson::object([
            ("ok", true.into()),
            (
                "value",
                IvyJson::Array(names.into_iter().map(IvyJson::String).collect()),
            ),
        ]),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    };
    append(IvyJson::object([("root",IvyJson::String(node_path::from_native(&path))),("actual",actual.clone()),("expected",expected.clone()),("scope","Exact original AST records supply only the named fixtures; production parser remains mandatory.".into())]));
    assert_eq!(&actual, expected.required("operation").unwrap());
    assert_eq!(
        *observed.lock().unwrap(),
        table
            .into_iter()
            .map(|(path, _, _)| path)
            .collect::<Vec<_>>()
    );
    actual
}
#[tokio::test]
async fn emitted_inventory_preserves_original_syntax_literal_names_breadth_first_and_set_order() {
    let fixture = Fixture::new();
    fixture.write("root.cjs",br#"import x from 'alpha/sub'; export {a} from '@scope/leaf/path'; require('beta'); import('gamma');
    function nested(require) { require(`template/sub`); }
    // require('ignored-comment');
    const text="require('ignored-string')"; require(variable); other.require('ignored-property');
    require('node:fs');require('./relative');require('/absolute');require('bad:name');require('@incomplete');
    require('ALPHA');require('alpha/again');require('_leading');require('with.dot/path');require('@s/p');
    require('bad!');require('p/\ud800');require('\ud800bad');require('@a/');require('');require('.hidden');"#);
    fixture.write("ignored.JS", b"require('wrong-extension')");
    fixture.write("ignored.js.map", b"require('wrong-map')");
    fixture.write(
        "nested/second.mjs",
        b"export * from 'deep';require('beta/sub');import('dynamic');",
    );
    fixture.write(
        "nested/third.js",
        b"const broken = ; require('recovered'); import x from 'last';",
    );
    fixture.write("directory.js/child.js", b"require('directory-child')");
    fixture.write("unicode-\u{1f600}.js", b"require('unicode-file')");
    fixture.write("replace.js", b"// \xff\nrequire('decoded-source')");
    assert_eq!(
        compare(fixture.0.clone()).await.get("ok"),
        Some(&true.into())
    );
}
#[tokio::test]
async fn emitted_inventory_skips_observed_directory_links_and_keeps_missing_errors() {
    let fixture = Fixture::new();
    let output = fixture.0.join("output");
    fs::create_dir(&output).unwrap();
    fixture.write("outside/hidden.js", b"require('outside')");
    fixture.write("output/ordinary.js", b"require('inside')");
    ivy_host_runtime::filesystem::directory_link(
        &fixture.0.join("outside"),
        &output.join("linked-directory"),
    )
    .unwrap();
    let value = compare(output).await;
    assert_eq!(
        value.required("value").unwrap(),
        &IvyJson::Array(vec!["inside".into()])
    );
    assert_eq!(
        fs::read(fixture.0.join("outside/hidden.js")).unwrap(),
        b"require('outside')"
    );
    compare(fixture.0.join("absent")).await;
    let file = fixture.0.join("plain");
    fs::write(&file, b"bytes").unwrap();
    compare(file).await;
}
#[tokio::test]
async fn admitted_emitted_inventory_keeps_running_after_its_waiter_is_dropped() {
    use std::{
        future::{Future, poll_fn},
        sync::{Condvar, mpsc},
        task::Poll,
        time::Duration,
    };
    let fixture = Fixture::new();
    fixture.write("first.js", b"first");
    fixture.write("second.js", b"second");
    let (started_tx, started) = mpsc::channel();
    let (finished_tx, finished) = mpsc::channel();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let callback_gate = gate.clone();
    let parser: JavaScriptImports = Arc::new(move |_, source| {
        if source == &JsString::from("first") {
            started_tx.send(()).unwrap();
            let (lock, ready) = &*callback_gate;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = ready.wait(released).unwrap();
            }
        } else {
            finished_tx.send(()).unwrap();
        }
        Ok(Vec::new())
    });
    let mut work = Box::pin(emitted_runtime_packages(fixture.0.clone(), parser));
    poll_fn(|cx| {
        assert!(matches!(work.as_mut().poll(cx), Poll::Pending));
        Poll::Ready(())
    })
    .await;
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(work);
    let (lock, ready) = &*gate;
    *lock.lock().unwrap() = true;
    ready.notify_all();
    finished.recv_timeout(Duration::from_secs(5)).unwrap();
    append(IvyJson::object([(
        "scope",
        "Second fixture parser call reached after dropping the admitted inventory waiter.".into(),
    )]));
}
