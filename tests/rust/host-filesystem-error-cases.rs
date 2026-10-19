use ivy_contracts::{IvyJson, JsString, json};
use ivy_host_runtime::{
    filesystem_error::{self, NodeFilesystemError, Syscall},
    node_path::{self, Context},
};
use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::Mutex,
};
static CORPUS: Mutex<()> = Mutex::new(());

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("ivy-filesystem-error-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self, name: &str) -> JsString {
        node_path::from_native(&self.0.join(name))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn oracle(input: IvyJson) -> IvyJson {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/host-filesystem-error-oracle.mjs");
    let mut child = Command::new("node")
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json::stringify(&input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
}
fn value(error: NodeFilesystemError) -> IvyJson {
    IvyJson::object([
        ("code", error.code.into()),
        ("errno", IvyJson::Number(error.errno as f64)),
        ("syscall", error.syscall.into()),
        ("path", IvyJson::String(error.path)),
        ("message", IvyJson::String(error.message)),
    ])
}
fn pair(case: &str, input: IvyJson, raw: Option<i32>, actual: IvyJson, original: IvyJson) {
    if let Some(file) = std::env::var_os("IVY_HOST_FILESYSTEM_ERROR_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(file)
                .unwrap(),
            "{}",
            json::stringify(
                &IvyJson::object([
                    ("case", case.into()),
                    ("input", input.clone()),
                    (
                        "nativeRawError",
                        raw.map(|r| IvyJson::Number(r as f64))
                            .unwrap_or(IvyJson::Null)
                    ),
                    ("actual", actual.clone()),
                    ("original", original.clone()),
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
    assert_eq!(
        json::canonical(&actual, usize::MAX).unwrap(),
        json::canonical(original.required("value").unwrap(), usize::MAX).unwrap(),
        "{case}: {input:?}; raw={raw:?}"
    );
}
fn compare(case: &str, syscall: Syscall, path: JsString) {
    let context = Context::current().unwrap();
    let physical = context.filesystem_path(&path);
    let error = match syscall {
        Syscall::Scandir => fs::read_dir(&physical).map(|_| ()).unwrap_err(),
        Syscall::Stat => fs::metadata(&physical).map(|_| ()).unwrap_err(),
        Syscall::Lstat => fs::symlink_metadata(&physical).map(|_| ()).unwrap_err(),
        Syscall::Rmdir => fs::remove_dir(&physical).unwrap_err(),
        Syscall::Unlink => fs::remove_file(&physical).unwrap_err(),
    };
    let actual = filesystem_error::format_at(&error, syscall, &path, &context).unwrap();
    assert_eq!(
        filesystem_error::format(&error, syscall, &path).unwrap(),
        Some(actual.clone())
    );
    let input = IvyJson::object([
        ("syscall", syscall.as_str().into()),
        ("path", IvyJson::String(path)),
    ]);
    pair(
        case,
        input.clone(),
        error.raw_os_error(),
        value(actual),
        oracle(input),
    );
}

#[test]
fn filesystem_error_messages_match_actual_node_io_and_path_boundaries() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("file"), b"owned").unwrap();
    fs::create_dir(fixture.0.join("nonempty")).unwrap();
    fs::write(fixture.0.join("nonempty/child"), b"owned").unwrap();
    for syscall in [
        Syscall::Scandir,
        Syscall::Stat,
        Syscall::Lstat,
        Syscall::Unlink,
        Syscall::Rmdir,
    ] {
        compare(
            &format!("missing-{}", syscall.as_str()),
            syscall,
            fixture.path("missing"),
        );
        compare(
            &format!("intermediate-file-{}", syscall.as_str()),
            syscall,
            fixture.path("file/child"),
        );
    }
    compare("scandir-file", Syscall::Scandir, fixture.path("file"));
    compare("rmdir-file", Syscall::Rmdir, fixture.path("file"));
    compare(
        "unlink-directory",
        Syscall::Unlink,
        fixture.path("nonempty"),
    );
    compare("rmdir-nonempty", Syscall::Rmdir, fixture.path("nonempty"));
    compare("empty-logical-path", Syscall::Stat, "".into());
    let context = Context::current().unwrap();
    let relative = context.relative(&context.cwd, &fixture.path("missing-é-'quote"));
    compare("relative-unicode-quoted-path", Syscall::Stat, relative);
    let mut surrogate = fixture.path("missing-");
    surrogate.0.extend([0xd800, 47, 0xdc00]);
    compare("utf8-surrogate-boundary", Syscall::Lstat, surrogate);
    #[cfg(windows)]
    {
        compare(
            "forward-slash-dot-normalization",
            Syscall::Stat,
            format!("{}/nonexistent/../missing", fixture.0.display())
                .replace('\\', "/")
                .into(),
        );
        compare(
            "explicit-namespace-path",
            Syscall::Stat,
            format!("\\\\?\\{}\\missing", fixture.0.display()).into(),
        );
    }
    assert!(fixture.0.join("file").is_file());
    assert!(fixture.0.join("nonempty/child").is_file());
}

#[cfg(windows)]
struct DeniedDirectory(PathBuf);
#[cfg(windows)]
impl DeniedDirectory {
    fn command(&self, args: &[&str]) {
        let tool =
            PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/icacls.exe");
        let output = Command::new(tool).arg(&self.0).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "Owned fixture ACL operation failed"
        );
    }
    fn new(path: PathBuf) -> Self {
        let owned = Self(path);
        owned.command(&["/deny", "*S-1-1-0:(RD)"]);
        owned
    }
}
#[cfg(windows)]
impl Drop for DeniedDirectory {
    fn drop(&mut self) {
        self.command(&["/remove:d", "*S-1-1-0"]);
    }
}

#[cfg(windows)]
#[test]
fn filesystem_error_windows_real_denied_and_sharing_failures_match_node() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("denied")).unwrap();
    {
        let _denied = DeniedDirectory::new(fixture.0.join("denied"));
        compare(
            "actual-acl-denied-scandir",
            Syscall::Scandir,
            fixture.path("denied"),
        );
    }
    assert!(fs::read_dir(fixture.0.join("denied")).is_ok());
    fs::write(fixture.0.join("locked"), b"owned").unwrap();
    {
        let _file = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(fixture.0.join("locked"))
            .unwrap();
        compare(
            "actual-sharing-denied-unlink",
            Syscall::Unlink,
            fixture.path("locked"),
        );
    }
    fs::remove_file(fixture.0.join("locked")).unwrap();
}

#[test]
fn filesystem_error_native_errno_table_matches_node_runtime_map() {
    #[cfg(windows)]
    let cases = vec![
        (2, Syscall::Stat, "ENOENT"),
        (267, Syscall::Scandir, "ENOTDIR"),
        (267, Syscall::Rmdir, "ENOENT"),
        (5, Syscall::Scandir, "EPERM"),
        (740, Syscall::Stat, "EACCES"),
        (32, Syscall::Unlink, "EBUSY"),
        (145, Syscall::Rmdir, "ENOTEMPTY"),
        (183, Syscall::Stat, "EEXIST"),
        (87, Syscall::Stat, "EINVAL"),
        (4, Syscall::Stat, "EMFILE"),
        (8, Syscall::Stat, "ENOMEM"),
        (206, Syscall::Stat, "ENAMETOOLONG"),
        (112, Syscall::Stat, "ENOSPC"),
        (19, Syscall::Unlink, "EROFS"),
        (1921, Syscall::Lstat, "ELOOP"),
        (1, Syscall::Scandir, "EISDIR"),
        (1117, Syscall::Stat, "EIO"),
    ];
    #[cfg(unix)]
    let cases = vec![
        (libc::ENOENT, Syscall::Stat, "ENOENT"),
        (libc::ENOTDIR, Syscall::Scandir, "ENOTDIR"),
        (libc::EPERM, Syscall::Unlink, "EPERM"),
        (libc::EACCES, Syscall::Scandir, "EACCES"),
        (libc::EBUSY, Syscall::Unlink, "EBUSY"),
        (libc::ENOTEMPTY, Syscall::Rmdir, "ENOTEMPTY"),
        (libc::EEXIST, Syscall::Stat, "EEXIST"),
        (libc::EINVAL, Syscall::Stat, "EINVAL"),
        (libc::EMFILE, Syscall::Stat, "EMFILE"),
        (libc::ENFILE, Syscall::Stat, "ENFILE"),
        (libc::ENOMEM, Syscall::Stat, "ENOMEM"),
        (libc::ENAMETOOLONG, Syscall::Stat, "ENAMETOOLONG"),
        (libc::ENOSPC, Syscall::Stat, "ENOSPC"),
        (libc::EROFS, Syscall::Unlink, "EROFS"),
        (libc::ELOOP, Syscall::Lstat, "ELOOP"),
        (libc::EISDIR, Syscall::Scandir, "EISDIR"),
        (libc::EIO, Syscall::Stat, "EIO"),
    ];
    let context = Context::current().unwrap();
    for (raw, syscall, code) in cases {
        let error = io::Error::from_raw_os_error(raw);
        let mapped =
            filesystem_error::format_at(&error, syscall, &"missing".into(), &context).unwrap();
        let original = oracle(IvyJson::object([("mapping", code.into())]));
        let prefix = format!("{}: ", mapped.code);
        let text = String::from_utf16(&mapped.message.0).unwrap();
        let description = text
            .strip_prefix(&prefix)
            .unwrap()
            .split(", ")
            .next()
            .unwrap();
        let actual = IvyJson::object([
            ("code", mapped.code.into()),
            ("errno", IvyJson::Number(mapped.errno as f64)),
            ("description", description.into()),
        ]);
        pair(
            "runtime-map-only",
            IvyJson::object([("syscall", syscall.as_str().into())]),
            Some(raw),
            actual,
            original,
        );
    }
}

#[test]
fn filesystem_error_unknown_and_argument_failures_have_no_fabricated_message() {
    for error in [
        io::Error::from_raw_os_error(1234567),
        io::Error::from_raw_os_error(0),
        io::Error::new(io::ErrorKind::PermissionDenied, "private native cause"),
    ] {
        assert!(
            filesystem_error::format(&error, Syscall::Stat, &"missing".into())
                .unwrap()
                .is_none()
        );
    }
    let known = fs::metadata(Fixture::new().0.join("missing")).unwrap_err();
    assert!(
        filesystem_error::format(&known, Syscall::Stat, &JsString(vec![0]))
            .unwrap()
            .is_none()
    );
}
