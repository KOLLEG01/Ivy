use super::{ServiceFixture, fixture_node, host_configuration};
use ivy_contracts::{IvyError, IvyJson as J};
use ivy_host_executor::{Bindings, Startup};
use ivy_host_runtime::{
    config,
    executor::{ExecutorFuture, ExecutorOptions},
    executor_lock::ExecutorLock,
    journal::HostJournal,
    package_updater::PackageClient,
    portable_http::PortableHttp,
    prepare::PreparationRuntime,
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

struct UnusedPackages;
impl PackageClient for UnusedPackages {
    fn catalog(&self, _: String, _: String, _: f64) -> ExecutorFuture<'_, Option<J>> {
        panic!("Constructor qualification does not make package requests")
    }
    fn download(&self, _: String, _: String, _: J, _: PathBuf) -> ExecutorFuture<'_, ()> {
        panic!("Constructor qualification does not download artifacts")
    }
}
fn preparation() -> PreparationRuntime {
    PreparationRuntime {
        node: Some(fixture_node("24.18.0")),
        compare: Arc::new(|_, _| panic!("No artifact inventory is admitted")),
        imports: Some(Arc::new(|_, _| panic!("No preparation/parser work is admitted"))),
        // Unused explicit test input; this does not qualify a transport profile.
        http: PortableHttp::new(Default::default()).unwrap(),
    }
}
fn options(f: &ServiceFixture) -> ExecutorOptions {
    ExecutorOptions {
        own_instance_id: Some("wrong-default".into()),
        account_home: f.0.to_string_lossy().to_string().into(),
        build_id: Some(format!("sha256:{}", "d".repeat(64)).into()),
    }
}
fn startup(f: &ServiceFixture, explicit: Option<&str>, configured: Option<&str>) -> Startup {
    let mut own = f.value();
    own.insert("instanceId", "renamed-executor".into());
    own.insert("componentId", "host-executor".into());
    own.remove("credential");
    if let Some(credential) = explicit {
        own.insert("credential", credential.into());
    }
    let mut host = host_configuration(f);
    let instance = |id: &str, credential: Option<&str>| {
        let mut value = J::object([
            ("instanceId", id.into()),
            ("serviceNodeId", id.into()),
            ("componentId", "host-executor".into()),
            ("engine", "process".into()),
            ("enabled", true.into()),
            ("settings", J::object([] as [(&str, J); 0])),
        ]);
        if let Some(credential) = credential {
            value.insert("credential", credential.into());
        }
        value
    };
    host.insert(
        "instances",
        J::Array(vec![
            instance("host-executor", Some("decoy-credential")),
            instance("renamed-executor", configured),
        ]),
    );
    host.insert(
        "packageUpdates",
        J::object([("intervalSeconds", J::Number(60.0))]),
    );
    Startup {
        host_path: f.0.join("host.json"),
        host,
        own: Some(config::InstanceConfig::from_json(own).unwrap()),
        launch_id: None,
    }
}
fn native(
    f: &ServiceFixture,
    startup: &Startup,
    preparation: PreparationRuntime,
    packages: Option<Arc<dyn PackageClient>>,
) -> Result<Bindings, IvyError> {
    Bindings::native_for_startup(
        startup,
        preparation,
        f.0.join("retained-current-package"),
        f.0.join("unused-ivy-job"),
        options(f),
        packages,
        1_700_000_000_123.0,
    )
}

#[tokio::test]
async fn native_factory_retains_configured_identity_credentials_and_explicit_bindings() {
    let f = ServiceFixture::new();
    // Use admitted nonempty credentials. A configured renamed executor must
    // never borrow the decoy's credential; a temporary unconfigured executor
    // retains the original first-component fallback.
    for (has_own, explicit, configured, package_setting, expects_packages) in [
        (true, Some("launch-credential"), None, true, true),
        (true, None, Some("seed-credential"), true, true),
        (true, None, None, true, false),
        (true, Some("launch-credential"), None, false, false),
        (false, None, None, true, true),
    ] {
        let mut startup = startup(&f, explicit, configured);
        if !has_own {
            startup.own = None;
        }
        if !package_setting {
            startup.host.remove("packageUpdates");
        }
        if expects_packages {
            let failure = native(&f, &startup, preparation(), None).err().unwrap();
            assert_eq!(failure.code, "unsupported_runtime");
            assert!(!f.0.join("state/executor").exists());
            assert!(!f.0.join("state/deployments.sqlite").exists());
        }
        let preparation = preparation();
        let compare = preparation.compare.clone();
        let node = preparation.node.clone();
        let packages: Arc<dyn PackageClient> = Arc::new(UnusedPackages);
        let bindings = native(
            &f,
            &startup,
            preparation,
            expects_packages.then(|| packages.clone()),
        )
        .unwrap();
        assert_eq!(
            bindings.options.own_instance_id.as_deref(),
            has_own.then_some("renamed-executor")
        );
        assert_eq!(bindings.options.account_home, options(&f).account_home);
        assert_eq!(bindings.options.build_id, options(&f).build_id);
        assert!(bindings.restarts.is_some());
        assert_eq!(bindings.packages.is_some(), expects_packages);
        if let Some(runtime) = bindings.packages {
            assert!(Arc::ptr_eq(&runtime.client, &packages));
            assert!(Arc::ptr_eq(&runtime.compare, &compare));
            assert_eq!(runtime.node.as_ref().unwrap().executable, node.as_ref().unwrap().executable);
            assert_eq!(runtime.node.as_ref().unwrap().version, node.as_ref().unwrap().version);
            assert_eq!(runtime.distribution, f.0.join("retained-current-package"));
            assert_eq!(runtime.job_launcher, f.0.join("unused-ivy-job"));
            let error = runtime
                .automatic
                .inspect(f.0.join("absent-config.json"), f.0.clone())
                .await
                .unwrap_err();
            assert_eq!(error.code, "internal_error");
        }
    }
}

#[tokio::test]
async fn native_factory_serves_an_empty_temporary_host_and_releases_ownership() {
    let f = ServiceFixture::new();
    let mut host = host_configuration(&f);
    host.insert("instances", J::Array(vec![]));
    let startup = Startup {
        host_path: f.0.join("host.json"),
        host: host.clone(),
        own: None,
        launch_id: None,
    };
    let bindings = native(&f, &startup, preparation(), None).unwrap();
    assert!(bindings.options.own_instance_id.is_none());
    assert!(bindings.packages.is_none());
    assert!(bindings.restarts.is_some());
    let stop = CancellationToken::new();
    let task = tokio::spawn(ivy_host_executor::serve(startup, bindings, stop.clone()));
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Ok(status) =
                config::json_file(f.0.join("state/executor.json"), 1024 * 1024).await
            {
                assert_eq!(status.get("hostId"), Some(&"fixture-host".into()));
                assert_eq!(
                    status.get("buildId"),
                    Some(&J::String(options(&f).build_id.unwrap()))
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let status = config::json_file(f.0.join("state/executor.json"), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(status.get("state"), Some(&"stopping".into()));
    let lock = ExecutorLock::acquire(f.0.join("state/executor"), false)
        .await
        .unwrap();
    lock.close().await.unwrap();
    let journal = HostJournal::open(host).await.unwrap();
    journal.close().await.unwrap();
}
