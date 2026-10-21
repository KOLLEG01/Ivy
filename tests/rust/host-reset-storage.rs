use ivy_contracts::{IvyError, IvyJson, JsString, Outcome, json::parse};
use ivy_hive::store::Store;
use ivy_host_executor::reset_storage::{ResetBootstrap, ResetStorage, hive_reset_storage};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct Temporary(PathBuf);
impl Temporary {
    fn new() -> Self {
        let parent = std::env::temp_dir();
        let path = parent.join(format!(
            "ivy-host-reset-storage-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert_eq!(path.parent(), Some(parent.as_path()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture() -> IvyJson {
    // Reuse the owner's existing raw instruction/root/contract inputs, without
    // executing its already qualified 29-trace differential corpus.
    let mut command =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()));
    command.current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .args(["--input-type=module", "--eval", "import {cases} from './tests/fixtures/hive-reset-storage-cases.mjs';process.stdout.write(JSON.stringify(cases().find(c=>c.name==='rebootstrap-success').steps[0]));"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "Owner fixture input: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
}
fn request(
    path: PathBuf,
    input: &IvyJson,
    now: impl Fn() -> ivy_contracts::IvyResult<JsString> + Send + Sync + 'static,
) -> ResetBootstrap {
    ResetBootstrap {
        path,
        epoch: input
            .required("epoch")
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone(),
        instructions: input.required("values").unwrap().clone(),
        scope_roots: input
            .required("ids")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_js_string().unwrap().clone())
            .collect(),
        service_contracts: input
            .required("definitions")
            .unwrap()
            .as_array()
            .unwrap()
            .to_vec(),
        now: Arc::new(now),
    }
}
fn bootstrapped(path: &Path) -> bool {
    let store = Store::open(path).unwrap();
    let roots: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM objects WHERE id IN ('scope-a','scope-b')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let contracts: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM contracts WHERE key='service/explicit'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    store.close().unwrap();
    roots == 2 && contracts == 1
}

fn stored_epoch(path: &Path) -> String {
    let store = Store::open(path).unwrap();
    let epoch = store.metadata("runtime_epoch").unwrap().unwrap();
    store.close().unwrap();
    epoch
}

#[tokio::test(flavor = "current_thread")]
async fn real_owner_arguments_late_clock_and_errors_cross_the_service_boundary() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<ResetStorage>();
    let temp = Temporary::new();
    let input = fixture();
    let binding = hive_reset_storage();
    let path = temp.0.join("hive.sqlite");
    assert_eq!(
        binding.saved_instructions(path.clone()).await.unwrap(),
        IvyJson::Array(vec![])
    );
    assert!(!path.exists());

    let runtime_thread = std::thread::current().id();
    let epoch = input.required("epoch").unwrap().as_str().unwrap();
    let at = input
        .required("at")
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let clock_calls = calls.clone();
    binding
        .rebootstrap(request(path.clone(), &input, move || {
            assert_ne!(std::thread::current().id(), runtime_thread);
            clock_calls.fetch_add(1, Ordering::SeqCst);
            Ok(at.clone())
        }))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(stored_epoch(&path), epoch);
    assert_eq!(
        binding.saved_instructions(path.clone()).await.unwrap(),
        input.required("values").unwrap().clone()
    );
    assert!(bootstrapped(&path));

    let failed = temp.0.join("clock-failure.sqlite");
    let clock_error = IvyError::new("fixture_clock", "Retain the original owner error.")
        .with_outcome(Outcome::Unknown)
        .with_details(IvyJson::object([("boundary", "root-clock".into())]));
    let returned_error = clock_error.clone();
    let error = binding
        .rebootstrap(request(failed.clone(), &input, move || {
            Err(returned_error.clone())
        }))
        .await
        .unwrap_err();
    assert_eq!(error, clock_error);
    // Inspect only after Hive closes its Store. The retained epoch/instructions
    // and absent bootstrap prove the clock failed after restore, without
    // reentering the exclusive storage owner from inside its clock callback.
    assert_eq!(stored_epoch(&failed), epoch);
    assert_eq!(
        binding.saved_instructions(failed.clone()).await.unwrap(),
        input.required("values").unwrap().clone()
    );
    assert!(!bootstrapped(&failed));
}

#[tokio::test(flavor = "current_thread")]
async fn dropped_result_waiter_does_not_cancel_admitted_hive_work() {
    let temp = Temporary::new();
    let input = fixture();
    let binding = hive_reset_storage();
    let path = temp.0.join("hive.sqlite");
    let entered = Arc::new(tokio::sync::Notify::new());
    let clock_entered = entered.clone();
    let (release, wait) = mpsc::channel();
    let wait = Mutex::new(wait);
    let at = input
        .required("at")
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone();
    let operation = request(path.clone(), &input, move || {
        clock_entered.notify_one();
        wait.lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        Ok(at.clone())
    });
    let observer = tokio::spawn(async move { binding.rebootstrap(operation).await });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let observed = path.clone();
        if tokio::task::spawn_blocking(move || bootstrapped(&observed))
            .await
            .unwrap()
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Accepted Hive operation did not finish."
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        hive_reset_storage().saved_instructions(path).await.unwrap(),
        input.required("values").unwrap().clone()
    );
}
