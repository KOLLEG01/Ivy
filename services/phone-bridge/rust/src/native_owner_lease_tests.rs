use super::*;
use ivy_contracts::json::{parse, stringify};
use std::{
    fs,
    io::{Read, Write},
    os::windows::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
const EPOCH: &str = "00112233-4455-6677-8899-aabbccddeeff";

#[test]
fn lease_fixture() {
    let Some(root) = std::env::var_os("IVY_PHONE_LEASE_ROOT") else {
        return;
    };
    let epoch = std::env::var("IVY_PHONE_LEASE_EPOCH").unwrap();
    let results = PathBuf::from(std::env::var_os("IVY_PHONE_LEASE_RESULT_ROOT").unwrap());
    for attempt in 0.. {
        let outcome = NativeOwnerLease::acquire(Path::new(&root), &epoch);
        let second = if outcome.is_ok() {
            std::env::var_os("IVY_PHONE_LEASE_SECOND_ROOT")
                .map(|root| NativeOwnerLease::acquire(Path::new(&root), &epoch).is_ok())
        } else {
            None
        };
        let result = stringify(
            &IvyJson::Object(vec![
                ("ok".into(), IvyJson::Bool(outcome.is_ok())),
                (
                    "second".into(),
                    second.map(IvyJson::Bool).unwrap_or(IvyJson::Null),
                ),
            ]),
            1024,
        )
        .unwrap();
        // Only plain identity data is returned: dropping it must not drop the lease.
        drop(outcome);
        fs::write(results.join(format!("result-{attempt}.json")), result).unwrap();
        if std::env::var_os("IVY_PHONE_LEASE_HOLD").is_none() {
            break;
        }
        let mut command = [0];
        if std::io::stdin().read_exact(&mut command).is_err() || command[0] != b'r' {
            break;
        }
    }
}

struct Root {
    path: PathBuf,
    parent: PathBuf,
}
impl Root {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = parent.join(format!(
            "ivy-phone-native-lease-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self { path, parent }
    }
    fn dir(&self, name: &str) -> PathBuf {
        let path = self.path.join(name);
        fs::create_dir(&path).unwrap();
        path
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let absolute = fs::canonicalize(&self.path).unwrap();
        assert_eq!(absolute.parent(), Some(self.parent.as_path()));
        assert!(
            absolute
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-phone-native-lease-")
        );
        fs::remove_dir_all(absolute).unwrap();
    }
}

struct Worker {
    child: Child,
    results: PathBuf,
}
impl Worker {
    fn start(
        original: bool,
        root: &Path,
        epoch: &str,
        results: PathBuf,
        hold: bool,
        second: Option<&Path>,
    ) -> Self {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let mut command = if original {
            let mut command =
                Command::new(std::env::var_os("IVY_POWERSHELL").unwrap_or("pwsh".into()));
            command
                .args(["-NoProfile", "-File"])
                .arg(repo.join("tests/fixtures/phone-native-owner-lease-oracle.ps1"));
            command
        } else {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.args([
                "--exact",
                "native_owner::lease_tests::lease_fixture",
                "--nocapture",
            ]);
            command
        };
        command
            .current_dir(results.parent().unwrap())
            .env("IVY_PHONE_LEASE_ROOT", root)
            .env("IVY_PHONE_LEASE_EPOCH", epoch)
            .env("IVY_PHONE_LEASE_RESULT_ROOT", &results)
            .env_remove("IVY_PHONE_LEASE_HOLD")
            .env_remove("IVY_PHONE_LEASE_SECOND_ROOT")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        if hold {
            command.env("IVY_PHONE_LEASE_HOLD", "1");
        }
        if let Some(second) = second {
            command.env("IVY_PHONE_LEASE_SECOND_ROOT", second);
        }
        Self {
            child: command.spawn().unwrap(),
            results,
        }
    }
    fn result(&mut self, attempt: usize) -> IvyJson {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(bytes) = fs::read(self.results.join(format!("result-{attempt}.json"))) {
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    if let Ok(result) = parse(text, 1024) {
                        return result;
                    }
                }
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                let mut error = String::new();
                self.child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut error)
                    .unwrap();
                panic!("Owned lease fixture exited before result: {status} {error}");
            }
            assert!(
                Instant::now() < until,
                "Owned lease fixture result unavailable"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn exit(&mut self) {
        drop(self.child.stdin.take());
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(Instant::now() < until, "Owned lease fixture did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn ok(result: &IvyJson) -> bool {
    result.get("ok").unwrap().as_bool().unwrap()
}
fn record(root: &Path) -> Vec<u8> {
    fs::read(root.join("phone-native-owner.json")).unwrap()
}
fn previous(pid: u32, time: u64) -> Vec<u8> {
    NativeOwner {
        epoch: EPOCH.into(),
        pid: pid as i32,
        creation_filetime: time as i64,
    }
    .encode()
    .unwrap()
}
fn unlocked(root: &Path) {
    drop(
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(root.join("phone-native.lock"))
            .unwrap(),
    );
}
fn trace(name: &str, rows: Vec<IvyJson>) {
    if let Some(root) = std::env::var_os("IVY_PHONE_NATIVE_LEASE_TRACE") {
        fs::create_dir_all(&root).unwrap();
        fs::write(
            Path::new(&root).join(format!("{name}.json")),
            stringify(&IvyJson::Array(rows), 1_000_000).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn actual_original_and_rust_lifetime_leases_collide_and_recover_after_process_exit() {
    let mut rows = Vec::new();
    for original in [false, true] {
        let root = Root::new();
        let owner = root.dir("owner");
        let other = root.dir("other");
        let mut first = Worker::start(
            original,
            &owner,
            EPOCH,
            root.dir("first-results"),
            true,
            Some(&other),
        );
        let admitted = first.result(0);
        assert!(ok(&admitted));
        assert_eq!(admitted.get("second").unwrap().as_bool(), Some(false));
        assert!(!other.join("phone-native.lock").exists());
        let saved = record(&owner);
        let identity = NativeOwner::parse(&saved).unwrap();
        assert_eq!(identity.pid(), first.child.id());
        assert!(identity.ensure_predecessor_exited().is_err());
        assert!(
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(0)
                .open(owner.join("phone-native.lock"))
                .is_err()
        );
        let mut collision = Worker::start(
            !original,
            &owner,
            "aabbccdd-0011-2233-4455-66778899aabb",
            root.dir("collision-results"),
            false,
            None,
        );
        assert!(!ok(&collision.result(0)));
        collision.exit();
        assert_eq!(record(&owner), saved);
        first.exit();
        assert!(identity.ensure_predecessor_exited().is_ok());
        let mut replacement = Worker::start(
            !original,
            &owner,
            EPOCH,
            root.dir("replacement-results"),
            true,
            None,
        );
        assert!(ok(&replacement.result(0)));
        let next = NativeOwner::parse(&record(&owner)).unwrap();
        assert_eq!(next.pid(), replacement.child.id());
        replacement.exit();
        unlocked(&owner);
        rows.push(IvyJson::Object(vec![
            ("originalFirst".into(), IvyJson::Bool(original)),
            (
                "first".into(),
                IvyJson::from(String::from_utf8(saved).unwrap()),
            ),
            (
                "replacement".into(),
                IvyJson::from(String::from_utf8(record(&owner)).unwrap()),
            ),
            ("collisionRefused".into(), IvyJson::Bool(true)),
            ("globalSecondDirectoryRefused".into(), IvyJson::Bool(true)),
        ]));
    }
    trace("lifetime", rows);
}

#[test]
fn actual_original_and_rust_refuse_unknown_records_and_keep_failed_admission_retryable() {
    let current =
        ivy_host_runtime::platform::windows_process::current_process_created_filetime().unwrap();
    let mut rows = Vec::new();
    for original in [false, true] {
        let root = Root::new();
        for (index, bytes) in [
            previous(std::process::id(), current),
            b"{".to_vec(),
            b"{}".to_vec(),
            vec![b' '; 4097],
        ]
        .into_iter()
        .enumerate()
        {
            let owner = root.dir(&format!("bad-{index}"));
            fs::write(owner.join("phone-native-owner.json"), &bytes).unwrap();
            let mut worker = Worker::start(
                original,
                &owner,
                EPOCH,
                root.dir(&format!("bad-results-{index}")),
                false,
                None,
            );
            assert!(!ok(&worker.result(0)));
            worker.exit();
            assert_eq!(record(&owner), bytes);
            unlocked(&owner);
        }
        for (name, bytes) in [
            ("absent", previous(i32::MAX as u32, 1)),
            ("mismatch", previous(std::process::id(), current ^ 1)),
        ] {
            let owner = root.dir(name);
            fs::write(owner.join("phone-native-owner.json"), bytes).unwrap();
            let mut worker = Worker::start(
                original,
                &owner,
                EPOCH,
                root.dir(&format!("{name}-results")),
                false,
                None,
            );
            assert!(ok(&worker.result(0)));
            worker.exit();
            assert_eq!(
                NativeOwner::parse(&record(&owner)).unwrap().pid(),
                worker.child.id()
            );
        }
        let owner = root.dir("retry");
        let old = previous(i32::MAX as u32, 1);
        fs::write(owner.join("phone-native-owner.json"), &old).unwrap();
        let temporary = owner.join(format!("phone-native-owner.json.{EPOCH}.tmp"));
        fs::write(&temporary, b"existing temporary").unwrap();
        let mut worker = Worker::start(
            original,
            &owner,
            EPOCH,
            root.dir("retry-results"),
            true,
            None,
        );
        assert!(!ok(&worker.result(0)));
        assert_eq!(record(&owner), old);
        assert_eq!(fs::read(&temporary).unwrap(), b"existing temporary");
        unlocked(&owner);
        fs::remove_file(&temporary).unwrap();
        worker
            .child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"r")
            .unwrap();
        assert!(ok(&worker.result(1)));
        assert!(!temporary.exists());
        worker.exit();
        unlocked(&owner);
        rows.push(IvyJson::Object(vec![
            ("original".into(), IvyJson::Bool(original)),
            ("refusedRecords".into(), IvyJson::Number(4.0)),
            ("absentAndMismatchedAllowed".into(), IvyJson::Bool(true)),
            (
                "sameProcessRetryAfterCreateNewFailure".into(),
                IvyJson::Bool(true),
            ),
        ]));
    }
    trace("records-retry", rows);
}

#[test]
fn actual_original_and_rust_keep_old_record_and_temporary_after_failed_replace() {
    let mut rows = Vec::new();
    for original in [false, true] {
        let root = Root::new();
        let owner = root.dir("owner");
        let old = previous(i32::MAX as u32, 1);
        fs::write(owner.join("phone-native-owner.json"), &old).unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ)
            .open(owner.join("phone-native-owner.json"))
            .unwrap();
        let mut worker = Worker::start(
            original,
            &owner,
            EPOCH,
            root.dir("first-results"),
            false,
            None,
        );
        assert!(!ok(&worker.result(0)));
        worker.exit();
        assert_eq!(record(&owner), old);
        unlocked(&owner);
        let temporary = owner.join(format!("phone-native-owner.json.{EPOCH}.tmp"));
        let unpublished = NativeOwner::parse(&fs::read(&temporary).unwrap()).unwrap();
        assert_eq!(unpublished.pid(), worker.child.id());
        drop(held);
        let mut retry = Worker::start(
            original,
            &owner,
            EPOCH,
            root.dir("second-results"),
            false,
            None,
        );
        assert!(!ok(&retry.result(0)));
        retry.exit();
        assert_eq!(record(&owner), old);
        unlocked(&owner);
        rows.push(IvyJson::Object(vec![
            ("original".into(), IvyJson::Bool(original)),
            ("publishedRecordUnchanged".into(), IvyJson::Bool(true)),
            (
                "validUnpublishedTemporaryPreserved".into(),
                IvyJson::Bool(true),
            ),
            ("sameEpochRetryRefused".into(), IvyJson::Bool(true)),
        ]));
    }
    trace("failed-replace", rows);
}

#[test]
fn actual_original_and_rust_validate_paths_and_keep_non_ascii_epoch_spelling() {
    let mut rows = Vec::new();
    for original in [false, true] {
        let root = Root::new();
        let valid = root.dir("valid");
        let file = root.path.join("file");
        fs::write(&file, b"file").unwrap();
        for (index, (owner, epoch)) in [
            (PathBuf::from("."), EPOCH),
            (root.path.join("missing"), EPOCH),
            (file, EPOCH),
            (valid.clone(), "bad"),
            (PathBuf::new(), EPOCH),
            (valid.clone(), "{00112233-4455-6677-8899-aabbccddeeff}"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut worker = Worker::start(
                original,
                &owner,
                epoch,
                root.dir(&format!("invalid-results-{index}")),
                false,
                None,
            );
            assert!(!ok(&worker.result(0)));
            worker.exit();
            assert!(!root.path.join("phone-native.lock").exists());
            assert!(!valid.join("phone-native.lock").exists());
        }
        let epoch = "\u{a0}00112233-4455-6677-8899-AABBCCDDEEFF\u{85}";
        let mut worker = Worker::start(
            original,
            &valid,
            epoch,
            root.dir("unicode-results"),
            false,
            None,
        );
        assert!(ok(&worker.result(0)));
        worker.exit();
        let saved = record(&valid);
        assert_eq!(NativeOwner::parse(&saved).unwrap().epoch(), epoch);
        assert!(String::from_utf8(saved).unwrap().contains("\\u00A0"));
        rows.push(IvyJson::Object(vec![
            ("original".into(), IvyJson::Bool(original)),
            ("invalidPathOrEpochCases".into(), IvyJson::Number(6.0)),
            (
                "unicodeEpochPreservedAndAsciiEscaped".into(),
                IvyJson::Bool(true),
            ),
        ]));
    }
    trace("validation", rows);
}
