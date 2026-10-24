use super::{ServiceFixture, fixture_node, host_configuration};
use ivy_host_runtime::{
    IvyJson, config,
    executor::{ActivationRuntime, ExecutorFuture, ExecutorOptions, OperationContext},
    executor_lock::ExecutorLock,
    journal::HostJournal,
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use ivy_host_executor as service;

#[derive(Default)]
struct Idle(AtomicBool);
impl ActivationRuntime for Idle {
    fn prepare<'a>(
        &'a self,
        _: &'a OperationContext,
        _: &'a IvyJson,
        _: &'a str,
    ) -> ExecutorFuture<'a, IvyJson> {
        Box::pin(async { panic!("No deployment admitted") })
    }
    fn compatibility<'a>(
        &'a self,
        _: &'a OperationContext,
        _: &'a str,
        _: Option<IvyJson>,
    ) -> ExecutorFuture<'a, ()> {
        Box::pin(async { panic!("No deployment admitted") })
    }
    fn reconcile<'a>(
        &'a self,
        _: &'a OperationContext,
        _: Option<IvyJson>,
        _: bool,
    ) -> ExecutorFuture<'a, ()> {
        Box::pin(async { panic!("No deployment admitted") })
    }
    fn dispose(&self) -> ExecutorFuture<'_, ()> {
        Box::pin(async move {
            self.0.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
}
fn bindings(f: &ServiceFixture, runtime: Arc<Idle>) -> service::Bindings {
    service::Bindings {
        activation: runtime,
        options: ExecutorOptions {
            own_instance_id: Some("wrong-default".into()),
            account_home: f.0.to_string_lossy().to_string().into(),
            build_id: None,
        },
        restarts: None,
        packages: None,
    }
}
fn startup(f: &ServiceFixture) -> service::Startup {
    let mut own = f.value();
    own.remove("credential");
    service::Startup {
        host_path: f.0.join("host.json"),
        host: host_configuration(f),
        own: Some(config::InstanceConfig::from_json(own).unwrap()),
        launch_id: Some("entry-launch".into()),
    }
}
async fn health(f: &ServiceFixture) -> IvyJson {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Ok(value) = config::json_file(f.0.join("data/health.json"), 1024 * 1024).await {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn admission_observes_instance_before_host_selection_and_rejects_reset_before_lock() {
    let f = ServiceFixture::new();
    let node = fixture_node("24.18.0");
    let host_path = f.0.join("host.json");
    let mut host = host_configuration(&f);
    host.insert(
        "executables",
        IvyJson::object([(
            "node",
            f.0.join("fixture-node")
                .to_string_lossy()
                .to_string()
                .into(),
        )]),
    );
    config::atomic_json(&host_path, host).await.unwrap();
    let args = vec![
        OsString::from("--config"),
        host_path.clone().into_os_string(),
        "--instance-config".into(),
        f.0.join("absent.json").into_os_string(),
    ];
    let error = service::admit(
        &args,
        &BTreeMap::new(),
        &"relative-unused-home".into(),
        &node,
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.code, "internal_error");
    let args = vec![
        OsString::from("--config"),
        host_path.clone().into_os_string(),
    ];
    let admitted = service::admit(
        &args,
        &BTreeMap::new(),
        &"relative-unused-home".into(),
        &node,
    )
    .await
    .unwrap();
    assert_eq!(admitted.host_path, host_path);
    let marker =
        ivy_host_runtime::runtime_maintenance::runtime_reset_marker(&admitted.host).unwrap();
    config::atomic_json(&marker, IvyJson::object([] as [(&str, IvyJson); 0]))
        .await
        .unwrap();
    let error = service::admit(
        &args,
        &BTreeMap::new(),
        &"relative-unused-home".into(),
        &node,
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.code, "maintenance_active");
    let lock = ExecutorLock::acquire(f.0.join("state/executor"), false)
        .await
        .unwrap();
    lock.close().await.unwrap();
}
#[tokio::test]
async fn service_health_control_closes_and_releases_the_actual_journal_and_lock() {
    let f = ServiceFixture::new();
    let runtime = Arc::new(Idle::default());
    let task = tokio::spawn(service::serve(
        startup(&f),
        bindings(&f, runtime.clone()),
        CancellationToken::new(),
    ));
    let observed = health(&f).await;
    assert_eq!(observed.get("ready"), Some(&true.into()));
    let control = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        (
            "instanceId",
            observed.required("instanceId").unwrap().clone(),
        ),
        ("bootId", observed.required("bootId").unwrap().clone()),
        ("launchId", observed.required("launchId").unwrap().clone()),
        ("action", "shutdown".into()),
        ("requestedAt", "2026-10-20T00:00:00.000Z".into()),
    ]);
    let mut wrong = control.clone();
    wrong.insert("bootId", "other-boot".into());
    config::atomic_json(f.0.join("data/control.json"), wrong)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1150)).await;
    assert!(!task.is_finished());
    config::atomic_json(f.0.join("data/control.json"), control)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(runtime.0.load(Ordering::SeqCst));
    assert_eq!(health(&f).await.get("ready"), Some(&false.into()));
    let lock = ExecutorLock::acquire(f.0.join("state/executor"), false)
        .await
        .unwrap();
    lock.close().await.unwrap();
    let journal = HostJournal::open(host_configuration(&f)).await.unwrap();
    journal.close().await.unwrap();
}
#[tokio::test]
async fn dropped_service_observer_retains_cleanup_and_missing_update_binding_never_locks() {
    let f = ServiceFixture::new();
    let runtime = Arc::new(Idle::default());
    let stop = CancellationToken::new();
    let task = tokio::spawn(service::serve(
        startup(&f),
        bindings(&f, runtime.clone()),
        stop.clone(),
    ));
    health(&f).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(8), async {
        while !runtime.0.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Disposal precedes lock close; wait on the durable stopping status before
    // checking exclusive acquisition rather than interpreting a mid-close race.
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Ok(lock) = ExecutorLock::acquire(f.0.join("state/executor"), false).await {
                lock.close().await.unwrap();
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let mut next = startup(&f);
    let mut own = next.own.take().unwrap().json().clone();
    own.insert("credential", "synthetic".into());
    next.own = Some(config::InstanceConfig::from_json(own).unwrap());
    next.host.insert(
        "packageUpdates",
        IvyJson::object([] as [(&str, IvyJson); 0]),
    );
    let error = service::serve(next, bindings(&f, runtime), CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, "unsupported_runtime");
    let lock = ExecutorLock::acquire(f.0.join("state/executor"), false)
        .await
        .unwrap();
    lock.close().await.unwrap();
}
