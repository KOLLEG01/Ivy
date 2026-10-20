use super::{TextFileError, node_path, paths};
use ivy_contracts::{IvyJson as J, JsString, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
};
static CORPUS: Mutex<()> = Mutex::new(());
pub struct Fixture {
    pub root: PathBuf,
    base: PathBuf,
}
impl Fixture {
    pub fn new() -> Self {
        let base = paths::realpath(&std::env::temp_dir()).unwrap();
        let root = base.join(format!("ivy-regular-text-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self { root, base }
    }
    pub fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
    pub fn reset(&self) {
        for name in ["input", "input.held"] {
            let path = self.path(name);
            if let Ok(meta) = fs::symlink_metadata(&path) {
                assert!(!meta.is_dir());
                fs::remove_file(path).unwrap();
            }
        }
        fs::write(self.path("input"), b"retained").unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert_eq!(self.root.parent(), Some(self.base.as_path()));
        assert!(
            self.root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-regular-text-")
        );
        if !std::thread::panicking() {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }
}
pub fn text(path: &Path) -> JsString {
    node_path::from_native(path)
}
pub fn value(result: Result<Option<JsString>, TextFileError>) -> J {
    match result {
        Ok(value) => J::object([
            ("ok", true.into()),
            ("value", value.map(J::String).unwrap_or(J::Null)),
        ]),
        Err(error) => J::object([
            ("ok", false.into()),
            ("name", error.name.into()),
            ("code", error.code.map(J::from).unwrap_or(J::Null)),
            ("message", J::String(error.message)),
        ]),
    }
}
pub fn request(operation: &str, directory: &Path, path: JsString, limit: usize) -> J {
    J::object([
        ("operation", operation.into()),
        ("directory", J::String(text(directory))),
        ("path", J::String(path)),
        ("limit", J::Number(limit as f64)),
    ])
}
pub fn oracle(input: &J) -> J {
    let mut child = Command::new("node")
        .arg("tests/fixtures/host-regular-text-oracle.mjs")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json::stringify(input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    json::parse(std::str::from_utf8(&result.stdout).unwrap(), usize::MAX).unwrap()
}
pub fn pair(label: &str, input: J, actual: J, expected: J) {
    if let Some(path) = std::env::var_os("IVY_HOST_REGULAR_TEXT_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap(),
            "{}",
            json::stringify(
                &J::object([
                    ("case", label.into()),
                    ("input", input),
                    ("actual", actual.clone()),
                    ("expected", expected.clone())
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
    assert_eq!(
        json::canonical(&actual, usize::MAX).unwrap(),
        json::canonical(&expected, usize::MAX).unwrap(),
        "{label}"
    );
}
pub fn blocked(label: &str, error: &std::io::Error) {
    if let Some(path) = std::env::var_os("IVY_HOST_REGULAR_TEXT_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap(),
            "{}",
            json::stringify(
                &J::object([
                    ("case", label.into()),
                    ("execution", "blocked".into()),
                    ("finding", J::Null),
                    ("message", error.to_string().into())
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
}
