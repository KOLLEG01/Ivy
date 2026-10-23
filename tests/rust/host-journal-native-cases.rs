// Native coverage of the existing journal admission, health and retention cases.
use super::{J, ServiceFixture, assert_json, id, journal, save, seed};
use ivy_contracts::core_schema::validate_host;
use ivy_host_runtime::{journal::HostJournal, layout::service_paths, node_path};
use ivy_sdk::clock::Clock;
use std::{collections::BTreeSet, sync::Arc, time::Instant};

const DATE: &str = "2026-01-01T00:00:00.000Z";
struct FixedClock;
impl Clock for FixedClock {
    fn utc_millis(&self) -> i64 {
        1_767_225_600_000
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
fn hash(letter: &str) -> J {
    format!("sha256:{}", letter.repeat(64)).into()
}
fn empty() -> J {
    J::object([] as [(&str, J); 0])
}
fn request(action: &str, operation: &str) -> J {
    J::object([
        ("action", action.into()),
        ("operationId", operation.into()),
        ("instanceId", "fixture".into()),
    ])
}
fn target(f: &ServiceFixture) -> J {
    J::object([
        ("schemaVersion", J::Number(1.0)),
        ("instanceId", "fixture".into()),
        ("revision", "launch".into()),
        ("candidateId", hash("a")),
        ("desired", "running".into()),
        (
            "configPath",
            J::String(node_path::from_native(&f.0.join("state/instance.json"))),
        ),
        ("requestedAt", DATE.into()),
    ])
}

#[tokio::test]
async fn native_journal_admission_and_updates_validate_before_commit() {
    let f = ServiceFixture::new();
    save(&f).await;
    seed(&f).await;
    let journal = journal(&f).await;
    let config = journal.config().await.unwrap();
    let mut deploy = request("deploy", "typed-deploy");
    deploy.insert("candidateId", hash("a"));
    // An optional native argument is absent through None, never a scalar coercion.
    assert_eq!(
        journal
            .accept(deploy.clone(), Some(false.into()))
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    let mut source = request("deploy", "typed-source");
    source.insert("source", "source".into());
    assert_eq!(
        journal.accept(source.clone(), None).await.unwrap_err().code,
        "source_snapshot_required"
    );
    assert!(journal.accept(source, Some(false.into())).await.is_err());
    assert!(journal.list(100.0).await.unwrap().is_empty());
    let accepted = journal.accept(deploy.clone(), None).await.unwrap();
    assert_json(
        &journal.accept(deploy.clone(), None).await.unwrap(),
        &accepted,
    );
    let deployment = id(&accepted).to_utf8().unwrap();
    journal
        .advance(&deployment, "prepared", "checking", empty())
        .await
        .unwrap();
    let draining = journal
        .advance(&deployment, "checking", "draining", empty())
        .await
        .unwrap();
    let selected = target(&f);
    for key in ["readiness", "errorCode", "target", "installed"] {
        for invalid in [false.into(), J::Number(0.0), "".into()] {
            let mut change = J::object([("target", selected.clone())]);
            change.insert(key, invalid);
            assert!(
                journal
                    .advance(&deployment, "draining", "activating", change)
                    .await
                    .is_err(),
                "{key}"
            );
            assert_json(&journal.get(&deployment).await.unwrap(), &draining);
            assert_eq!(journal.target("fixture").await.unwrap(), None);
            assert_eq!(
                journal
                    .accepted_target_policy(selected.clone())
                    .await
                    .unwrap(),
                None
            );
        }
    }
    let activating = journal
        .advance(
            &deployment,
            "draining",
            "activating",
            J::object([
                ("target", selected.clone()),
                (
                    "readiness",
                    J::object([("state", "not_run".into()), ("message", "".into())]),
                ),
            ]),
        )
        .await
        .unwrap();
    validate_host("JournalEntry", &activating).unwrap();
    assert_json(
        &journal
            .accepted_target_policy(selected.clone())
            .await
            .unwrap()
            .unwrap(),
        &config,
    );
    journal
        .advance(&deployment, "activating", "verifying", empty())
        .await
        .unwrap();
    let installed = journal.installed("fixture").await.unwrap().unwrap();
    let completed = journal
        .advance(
            &deployment,
            "verifying",
            "succeeded",
            J::object([
                ("installed", installed),
                ("observedBuild", hash("a")),
                (
                    "readiness",
                    J::object([("state", "passed".into()), ("message", "".into())]),
                ),
            ]),
        )
        .await
        .unwrap();
    assert_eq!(journal.target("fixture").await.unwrap(), Some(selected));
    journal.close().await.unwrap();
    let reopened = HostJournal::open(config).await.unwrap();
    assert_json(&reopened.accept(deploy, None).await.unwrap(), &completed);
    reopened.close().await.unwrap();
}

fn health() -> J {
    J::object([
        ("schemaVersion", J::Number(1.0)),
        ("instanceId", "fixture".into()),
        ("componentId", "fixture".into()),
        ("buildId", hash("a")),
        ("pid", J::Number(123.0)),
        ("bootId", "boot".into()),
        ("startedAt", DATE.into()),
        ("observedAt", DATE.into()),
        ("ready", true.into()),
        ("generation", J::Null),
        ("launchId", "launch".into()),
        ("details", "".into()),
    ])
}

#[tokio::test]
async fn native_journal_health_requires_current_boolean_readiness_and_matching_launch() {
    let f = ServiceFixture::new();
    save(&f).await;
    seed(&f).await;
    let mut config = ivy_host_runtime::host_config::host_config(f.0.join("host.json"))
        .await
        .unwrap();
    let mut instances = config
        .required("instances")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    let mut executor_instance = instances[0].clone();
    executor_instance.insert("instanceId", "renamed-executor".into());
    executor_instance.insert("serviceNodeId", "host.runtime".into());
    executor_instance.insert("componentId", "host-executor".into());
    instances.push(executor_instance);
    config.insert("instances", J::Array(instances));
    let journal = HostJournal::open_with_clock(config.clone(), Arc::new(FixedClock))
        .await
        .unwrap();
    let selected = target(&f);
    journal.restore_target(selected.clone()).await.unwrap();
    let data = node_path::to_native(
        service_paths(&config, &"fixture".into())
            .unwrap()
            .required("data")
            .unwrap()
            .as_js_string()
            .unwrap(),
    );
    for (key, value, state, code) in [
        ("ready", true.into(), "ready", J::Null),
        (
            "ready",
            false.into(),
            "starting",
            "service_not_ready".into(),
        ),
        (
            "launchId",
            "another-launch".into(),
            "unknown",
            "build_mismatch".into(),
        ),
        ("buildId", hash("b"), "unknown", "build_mismatch".into()),
        (
            "observedAt",
            "2025-12-31T23:59:45.000Z".into(),
            "unknown",
            "health_stale".into(),
        ),
        (
            "observedAt",
            "2026-01-01T00:00:01.001Z".into(),
            "unknown",
            "health_stale".into(),
        ),
    ] {
        let mut owner = health();
        owner.insert(key, value);
        validate_host("Health", &owner).unwrap();
        ivy_host_runtime::config::atomic_json(data.join("health.json"), owner)
            .await
            .unwrap();
        let observations = journal.current_observations(None).await.unwrap();
        let observed = observations.required("fixture").unwrap();
        assert_eq!(observed.get("state"), Some(&J::from(state)), "{key}");
        assert_eq!(observed.get("code"), Some(&code), "{key}");
        if state == "starting" {
            assert_eq!(
                observed.get("message"),
                Some(&J::from("Owner health is not ready."))
            );
        }
    }
    // Legacy health without a launch ID remains supported; public message limits stay unchanged.
    let mut owner = health();
    owner.remove("launchId");
    owner.insert("details", "😀".repeat(400).into());
    ivy_host_runtime::config::atomic_json(data.join("health.json"), owner)
        .await
        .unwrap();
    let observations = journal.current_observations(None).await.unwrap();
    let observed = observations.required("fixture").unwrap();
    assert_eq!(observed.get("state"), Some(&J::from("ready")));
    journal
        .record_observation("fixture", observed.clone())
        .await
        .unwrap();
    let status = journal.status(None).await.unwrap();
    assert_eq!(
        status.required("instances").unwrap().as_array().unwrap()[0]
            .required("message")
            .unwrap()
            .as_js_string()
            .unwrap()
            .0
            .len(),
        512
    );
    for invalid in ["true".into(), J::Number(1.0)] {
        let mut owner = health();
        owner.insert("ready", invalid);
        ivy_host_runtime::config::atomic_json(data.join("health.json"), owner)
            .await
            .unwrap();
        assert_eq!(
            journal
                .current_observations(None)
                .await
                .unwrap()
                .required("fixture")
                .unwrap()
                .get("state"),
            Some(&J::from("unknown"))
        );
    }
    let mut stopped = selected;
    stopped.insert("desired", "stopped".into());
    journal.restore_target(stopped).await.unwrap();
    ivy_host_runtime::config::atomic_json(data.join("health.json"), health())
        .await
        .unwrap();
    let observations = journal.current_observations(None).await.unwrap();
    assert_eq!(
        observations.required("fixture").unwrap().get("state"),
        Some(&J::from("stopped"))
    );
    assert_eq!(
        observations.required("fixture").unwrap().get("code"),
        Some(&J::Null)
    );
    let executor = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("hostId", "fixture-host".into()),
        ("pid", J::Number(123.0)),
        ("bootId", "boot".into()),
        ("buildId", hash("a")),
        ("observedAt", DATE.into()),
        ("state", "ready".into()),
        ("activeDeploymentId", J::Null),
        ("code", J::Null),
    ]);
    for (build, code, state, message) in [
        (hash("a"), J::Null, "ready", ""),
        (
            hash("a"),
            "runtime_failed".into(),
            "ready",
            "Host executor reported runtime_failed.",
        ),
        (
            J::Null,
            J::Null,
            "unknown",
            "Running executor release does not match the installed release.",
        ),
    ] {
        let mut status = executor.clone();
        status.insert("buildId", build);
        status.insert("code", code);
        validate_host("ExecutorStatus", &status).unwrap();
        let observations = journal.current_observations(Some(status)).await.unwrap();
        let observed = observations.required("renamed-executor").unwrap();
        assert_eq!(observed.get("state"), Some(&J::from(state)));
        assert_eq!(observed.get("message"), Some(&J::from(message)));
    }
    journal.close().await.unwrap();
}

#[tokio::test]
async fn native_journal_retention_protects_active_and_exact_rollback_candidates() {
    let f = ServiceFixture::new();
    save(&f).await;
    seed(&f).await;
    let journal = journal(&f).await;
    let config = journal.config().await.unwrap();
    let base_candidate = journal
        .candidate(&hash("a").as_str().unwrap())
        .await
        .unwrap();
    let base_manifest = journal
        .manifest(&hash("a").as_str().unwrap())
        .await
        .unwrap();
    for (letter, version) in [
        ("b", "2.0.0"),
        ("c", "2.0.0"),
        ("d", "3.0.0"),
        ("e", "4.0.0"),
    ] {
        let mut candidate = base_candidate.clone();
        candidate.insert("candidateId", hash(letter));
        candidate.insert("buildId", hash(letter));
        let mut manifest = base_manifest.clone();
        manifest.insert("buildId", hash(letter));
        manifest.insert("version", version.into());
        journal.save_candidate(candidate, manifest).await.unwrap();
    }
    let mut installed = journal.installed("fixture").await.unwrap().unwrap();
    installed.insert("candidateId", hash("d"));
    installed.insert("buildId", hash("d"));
    journal.set_installed(installed.clone()).await.unwrap();
    let ids = |letters: &[&str]| -> BTreeSet<String> {
        letters.iter().map(|v| hash(v).as_str().unwrap()).collect()
    };
    assert_eq!(
        journal.retention_candidate_ids().await.unwrap(),
        ids(&["c", "d"])
    );
    let mut rollback = request("rollback", "typed-rollback");
    rollback.insert("targetBuild", hash("a"));
    let accepted = journal.accept(rollback, None).await.unwrap();
    let deployment = id(&accepted).to_utf8().unwrap();
    assert_eq!(
        journal.retention_candidate_ids().await.unwrap(),
        ids(&["a", "c", "d"])
    );
    assert!(
        !journal
            .retire_candidate(&hash("a").as_str().unwrap())
            .await
            .unwrap()
    );
    for (from, to) in [
        ("prepared", "checking"),
        ("checking", "draining"),
        ("draining", "activating"),
        ("activating", "verifying"),
    ] {
        journal
            .advance(&deployment, from, to, empty())
            .await
            .unwrap();
    }
    installed.insert("candidateId", hash("a"));
    installed.insert("buildId", hash("a"));
    journal
        .advance(
            &deployment,
            "verifying",
            "succeeded",
            J::object([("installed", installed)]),
        )
        .await
        .unwrap();
    assert_eq!(
        journal.retention_candidate_ids().await.unwrap(),
        ids(&["a", "d"])
    );
    assert!(
        !journal
            .retire_candidate(&hash("d").as_str().unwrap())
            .await
            .unwrap()
    );
    assert!(
        journal
            .retire_candidate(&hash("c").as_str().unwrap())
            .await
            .unwrap()
    );
    journal.close().await.unwrap();
    let reopened = HostJournal::open(config).await.unwrap();
    assert_eq!(
        reopened.retention_candidate_ids().await.unwrap(),
        ids(&["a", "d"])
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn native_bootstrap_readiness_checks_boolean_owner_state_and_current_health() {
    use ivy_contracts::hash::hash_json;
    use ivy_host_runtime::bootstrap_auto_worker::{BootstrapReadiness, NativeBootstrapReadiness};
    let f = ServiceFixture::new();
    let source = save(&f).await;
    seed(&f).await;
    let journal = journal(&f).await;
    let config = journal.config().await.unwrap();
    let candidate = journal
        .candidate(&hash("a").as_str().unwrap())
        .await
        .unwrap();
    journal.close().await.unwrap();
    let identity = ivy_host_runtime::linux_resources::installation_identity(&config).unwrap();
    let plan = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("hostId", config.required("hostId").unwrap().clone()),
        ("os", if cfg!(windows) { "win32" } else { "linux" }.into()),
        ("installationId", identity.clone().into()),
        ("candidateId", hash("a")),
        (
            "artifactRoot",
            candidate.required("artifactRoot").unwrap().clone(),
        ),
        ("configPath", J::String(node_path::from_native(&source))),
        ("configHash", hash_json(&config).unwrap().into()),
        (
            "runtimeRoot",
            config.required("runtimeRoot").unwrap().clone(),
        ),
        (
            "nodeExecutable",
            config
                .required("executables")
                .unwrap()
                .required("node")
                .unwrap()
                .clone(),
        ),
        (
            "processes",
            J::Array(vec![J::object([
                ("instanceId", "fixture".into()),
                ("componentId", "fixture".into()),
                ("name", "fixture-owner".into()),
                ("candidateId", hash("a")),
            ])]),
        ),
    ]);
    validate_host("BootstrapPlan", &plan).unwrap();
    let states = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("hostId", config.required("hostId").unwrap().clone()),
        ("installationId", identity.into()),
        ("configPath", J::String(node_path::from_native(&source))),
        ("configurationHash", hash_json(&config).unwrap().into()),
        ("observedAt", DATE.into()),
        (
            "owners",
            J::Array(vec![J::object([
                ("instanceId", "fixture".into()),
                ("componentId", "fixture".into()),
                ("name", "fixture-owner".into()),
                ("candidateId", hash("a")),
                ("definitionHash", hash("b")),
                ("enabled", true.into()),
                ("running", true.into()),
            ])]),
        ),
    ]);
    validate_host("BootstrapSnapshot", &states).unwrap();
    let data = node_path::to_native(
        service_paths(&config, &"fixture".into())
            .unwrap()
            .required("data")
            .unwrap()
            .as_js_string()
            .unwrap(),
    );
    let readiness = NativeBootstrapReadiness::new(Arc::new(FixedClock));
    for (key, value, expected) in [
        ("ready", true.into(), None),
        ("ready", false.into(), Some("readiness_failed")),
        ("ready", J::Number(1.0), Some("readiness_failed")),
        ("buildId", hash("b"), Some("readiness_failed")),
        ("observedAt", "invalid".into(), Some("readiness_failed")),
        (
            "observedAt",
            "2025-12-31T23:59:44.999Z".into(),
            Some("readiness_failed"),
        ),
    ] {
        let mut health = health();
        health.insert(key, value);
        ivy_host_runtime::config::atomic_json(data.join("health.json"), health)
            .await
            .unwrap();
        let result = readiness
            .wait(
                config.clone(),
                plan.clone(),
                states.clone(),
                FixedClock.utc_millis(),
                0,
            )
            .await;
        assert_eq!(
            result.err().map(|error| error.code),
            expected.map(str::to_owned),
            "{key}"
        );
    }
    std::fs::remove_file(data.join("health.json")).unwrap();
    for (running, expected) in [
        (false.into(), None),
        (J::Number(0.0), Some("invalid_arguments")),
        (J::Null, Some("invalid_arguments")),
        ("".into(), Some("invalid_arguments")),
    ] {
        let mut states = states.clone();
        let mut owners = states
            .required("owners")
            .unwrap()
            .as_array()
            .unwrap()
            .to_vec();
        owners[0].insert("running", running);
        states.insert("owners", J::Array(owners));
        let result = readiness
            .wait(
                config.clone(),
                plan.clone(),
                states,
                FixedClock.utc_millis(),
                0,
            )
            .await;
        assert_eq!(
            result.err().map(|error| error.code),
            expected.map(str::to_owned)
        );
    }
    let database =
        ivy_store::rusqlite::Connection::open(f.0.join("state/deployments.sqlite")).unwrap();
    assert_eq!(
        database
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "delete"
    );
}
