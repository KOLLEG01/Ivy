use super::{Fixture, bootstrap, encoded, host, json, node, now};
use ivy_contracts::{IvyJson, hash::hash_json};
use ivy_host_runtime::{
    accepted_configuration::save_accepted_configuration,
    journal::HostJournal,
    layout::service_paths,
    policy_import::{self, PolicyImportRequest, ReviewedPolicyBinding},
    runtime_owner::RuntimeOwner,
};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

fn original(script: &str, input: IvyJson) -> IvyJson {
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            script,
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(encoded(&input).as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    ivy_contracts::json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
}
fn digest(tag: &str) -> IvyJson {
    format!("sha256:{}", tag.repeat(64)).into()
}
fn equal(left: &IvyJson, right: &IvyJson) {
    assert_eq!(encoded(left), encoded(right));
}
fn path(value: &IvyJson) -> PathBuf {
    PathBuf::from(value.as_str().unwrap())
}

async fn observed(journal: &HostJournal, id: &str, state: &str, revision: &IvyJson) -> IvyJson {
    let mut last = IvyJson::Null;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            last = journal.observations().await.unwrap();
            if let Some(value) = last.get(id).filter(|value| {
                value.get("state") == Some(&state.into())
                    && value.get("targetRevision") == Some(revision)
            }) {
                break value.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("Missing {id}/{state}: {}", encoded(&last)))
}

#[tokio::test]
async fn explicit_import_of_a_journal_copy_supports_strict_owners_and_retained_rollback() {
    let root = Fixture::new();
    let mut accepted = host(&root);
    let mut instances = accepted
        .required("instances")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    for id in ["stopped-fixture", "bootstrap-fixture"] {
        let mut instance = instances[0].clone();
        instance.insert("instanceId", id.into());
        instance.insert("serviceNodeId", id.into());
        instances.push(instance);
    }
    accepted.insert("instances", IvyJson::Array(instances.clone()));
    let runtime = root.0.join("state");
    let mut candidates = Vec::new();
    let mut files = Vec::new();
    for tag in ["a", "b"] {
        let artifact = root.0.join("artifacts").join(tag);
        let candidate = IvyJson::object([
            ("candidateId", digest(tag)),
            ("componentId", "fixture".into()),
            ("buildId", digest(tag)),
            ("createdAt", now().into()),
            (
                "artifactRoot",
                artifact.to_string_lossy().to_string().into(),
            ),
            (
                "manifestPath",
                artifact
                    .join("manifest.json")
                    .to_string_lossy()
                    .to_string()
                    .into(),
            ),
            (
                "platform",
                json(r#"{"os":"win32","arch":"x64","node":"24.18.0"}"#),
            ),
        ]);
        let manifest = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("componentId", "fixture".into()),
            ("kind", "service".into()),
            ("version", "1.0.0".into()),
            ("buildId", digest(tag)),
            ("connectsToHive", false.into()),
            (
                "requirements",
                json(r#"{"node":">=24.18.0 <25.0.0","hiveProtocol":null,"contracts":[]}"#),
            ),
            (
                "entrypoint",
                json(
                    r#"{"executable":"fixture","args":["--exact","owner_cases::owner_child_fixture","--nocapture"],"timeoutMs":10000}"#,
                ),
            ),
            (
                "readiness",
                json(
                    r#"{"timeoutMs":3000,"command":{"executable":"fixture","args":["--exact","owner_cases::owner_probe_fixture","--nocapture"],"timeoutMs":1000}}"#,
                ),
            ),
            ("shutdown", json(r#"{"timeoutMs":1000}"#)),
            (
                "restart",
                json(r#"{"policy":"never","minimumDelayMs":100,"maximumDelayMs":1000}"#),
            ),
            (
                "storage",
                json(r#"{"writeFormat":1,"minReadableFormat":1,"maxReadableFormat":1}"#),
            ),
        ]);
        files.push(candidate.required("manifestPath").unwrap().clone());
        files.push(
            artifact
                .join("dist/build-info.json")
                .to_string_lossy()
                .to_string()
                .into(),
        );
        candidates.push(IvyJson::Array(vec![candidate, manifest]));
    }
    let mut targets = Vec::new();
    let mut configurations = Vec::new();
    for (id, revision, tag, desired) in [
        ("fixture", "current-launch".to_string(), "a", "running"),
        (
            "stopped-fixture",
            "stopped-launch".to_string(),
            "a",
            "stopped",
        ),
        (
            "bootstrap-fixture",
            format!("sha256:{}", "c".repeat(64)),
            "a",
            "running",
        ),
        ("fixture", "rollback-launch".to_string(), "b", "running"),
        (
            "bootstrap-fixture",
            "old-bootstrap-launch".to_string(),
            "b",
            "running",
        ),
    ] {
        let instance = instances
            .iter()
            .find(|value| value.get("instanceId") == Some(&id.into()))
            .unwrap();
        let storage = service_paths(&accepted, instance).unwrap();
        let artifact = root.0.join("artifacts").join(tag);
        let config_path = runtime
            .join("instances")
            .join(id)
            .join("configurations")
            .join(revision.replace(':', "-"))
            .join("config.json");
        let target = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("instanceId", id.into()),
            ("revision", revision.clone().into()),
            ("candidateId", digest(tag)),
            ("desired", desired.into()),
            (
                "configPath",
                config_path.to_string_lossy().to_string().into(),
            ),
            ("requestedAt", now().into()),
        ]);
        let configuration = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("hostId", accepted.required("hostId").unwrap().clone()),
            ("instanceId", id.into()),
            (
                "serviceNodeId",
                instance.required("serviceNodeId").unwrap().clone(),
            ),
            ("componentId", "fixture".into()),
            (
                "publicBaseUrl",
                accepted.required("publicBaseUrl").unwrap().clone(),
            ),
            ("version", "1.0.0".into()),
            ("buildId", digest(tag)),
            (
                "artifactRoot",
                artifact.to_string_lossy().to_string().into(),
            ),
            ("dataRoot", storage.required("data").unwrap().clone()),
            (
                "settings",
                IvyJson::object([("retainedValue", revision.into())]),
            ),
        ]);
        files.push(target.required("configPath").unwrap().clone());
        targets.push(target);
        configurations.push(IvyJson::Array(vec![
            config_path.to_string_lossy().to_string().into(),
            configuration,
        ]));
    }
    let seed = original(
        "tests/fixtures/host-policy-import-fixture.mjs",
        IvyJson::object([
            ("mode", "seed".into()),
            ("host", accepted.clone()),
            ("candidates", IvyJson::Array(candidates)),
            ("targets", IvyJson::Array(targets.clone())),
            ("configurations", IvyJson::Array(configurations)),
        ]),
    );
    let reference = path(seed.required("reference").unwrap());
    files.push(seed.required("reference").unwrap().clone());
    for instance in &instances {
        let data = path(
            service_paths(&accepted, instance)
                .unwrap()
                .required("data")
                .unwrap(),
        );
        std::fs::create_dir_all(&data).unwrap();
        let file = data.join("retained.txt");
        std::fs::write(&file, "retained application values\n").unwrap();
        files.push(file.to_string_lossy().to_string().into());
    }
    let database = runtime.join("deployments.sqlite");
    let snapshot = |mode: &str| {
        original(
            "tests/fixtures/service-manager-policy-copy.mjs",
            IvyJson::object([
                ("mode", mode.into()),
                ("database", database.to_string_lossy().to_string().into()),
                ("files", IvyJson::Array(files.clone())),
            ]),
        )
    };
    let before = snapshot("checkpoint");
    let source = runtime.join("original-before-import.sqlite");
    std::fs::copy(&database, &source).unwrap();
    let source_bytes = std::fs::read(&source).unwrap();
    std::fs::copy(&source, &database).unwrap();
    let mut desired = accepted.clone();
    desired.insert(
        "executables",
        IvyJson::object([(
            "fixture",
            root.0
                .join("missing-desired.exe")
                .to_string_lossy()
                .to_string()
                .into(),
        )]),
    );
    let plan = policy_import::preview_target_policies(desired.clone())
        .await
        .unwrap();
    assert_eq!(plan.get("unassociatedTargets"), Some(&IvyJson::Number(5.0)));
    equal(&snapshot("snapshot"), &before);
    let reviewed = targets
        .iter()
        .map(|target| ReviewedPolicyBinding {
            target: target.clone(),
            configuration_path: reference.clone(),
            configuration_hash: hash_json(&accepted).unwrap(),
        })
        .collect::<Vec<_>>();
    let request = PolicyImportRequest {
        import_id: "incomplete-batch".into(),
        expected_state_hash: plan.required("stateHash").unwrap().as_str().unwrap(),
        bindings: reviewed[..1].to_vec(),
        require_complete: true,
    };
    assert_eq!(
        policy_import::import_target_policies(desired.clone(), request.clone())
            .await
            .unwrap_err()
            .code,
        "configuration_changed"
    );
    equal(
        &policy_import::preview_target_policies(desired.clone())
            .await
            .unwrap(),
        &plan,
    );
    equal(&snapshot("snapshot"), &before);
    let partial_request = PolicyImportRequest {
        import_id: "reviewed-partial".into(),
        require_complete: false,
        ..request.clone()
    };
    let partial = policy_import::import_target_policies(desired.clone(), partial_request.clone())
        .await
        .unwrap();
    assert_eq!(
        partial
            .required("remainingTargets")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        4
    );
    equal(
        &policy_import::import_target_policies(desired.clone(), partial_request)
            .await
            .unwrap(),
        &partial,
    );
    let stale = PolicyImportRequest {
        import_id: "stale-batch".into(),
        bindings: reviewed.clone(),
        ..request
    };
    assert_eq!(
        policy_import::import_target_policies(desired.clone(), stale)
            .await
            .unwrap_err()
            .code,
        "revision_conflict"
    );
    let remaining = policy_import::preview_target_policies(desired.clone())
        .await
        .unwrap();
    let complete_request = PolicyImportRequest {
        import_id: "reviewed-complete".into(),
        expected_state_hash: remaining.required("stateHash").unwrap().as_str().unwrap(),
        bindings: reviewed[1..].to_vec(),
        require_complete: true,
    };
    let receipt = policy_import::import_target_policies(desired.clone(), complete_request.clone())
        .await
        .unwrap();
    assert!(
        receipt
            .required("remainingTargets")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    equal(
        &policy_import::import_target_policies(desired.clone(), complete_request.clone())
            .await
            .unwrap(),
        &receipt,
    );
    let alternative = save_accepted_configuration(&desired).unwrap();
    let rebind_plan = policy_import::preview_target_policies(desired.clone())
        .await
        .unwrap();
    let rebind = PolicyImportRequest {
        import_id: "refused-rebind".into(),
        expected_state_hash: rebind_plan.required("stateHash").unwrap().as_str().unwrap(),
        bindings: vec![ReviewedPolicyBinding {
            target: targets[0].clone(),
            configuration_path: alternative,
            configuration_hash: hash_json(&desired).unwrap(),
        }],
        require_complete: false,
    };
    assert_eq!(
        policy_import::import_target_policies(desired.clone(), rebind)
            .await
            .unwrap_err()
            .code,
        "target_conflict"
    );
    equal(&snapshot("snapshot"), &before);
    assert_eq!(std::fs::read(&source).unwrap(), source_bytes);
    let mut expected = seed.clone();
    expected.insert("reference", IvyJson::Null);
    equal(
        &original(
            "tests/fixtures/host-policy-import-fixture.mjs",
            IvyJson::object([("mode", "inspect".into()), ("host", desired.clone())]),
        ),
        &expected,
    );
    let journal = HostJournal::open(desired.clone()).await.unwrap();
    for target in &targets {
        equal(
            &journal
                .accepted_target_policy(target.clone())
                .await
                .unwrap()
                .unwrap(),
            &accepted,
        );
    }
    let bootstrap = bootstrap(&root);
    let mut owners = Vec::new();
    for id in ["fixture", "stopped-fixture", "bootstrap-fixture"] {
        let owner = RuntimeOwner::for_host(desired.clone(), id.into(), bootstrap.clone(), node())
            .await
            .unwrap();
        owner.start().unwrap();
        owners.push(owner);
    }
    let initial = observed(
        &journal,
        "fixture",
        "ready",
        targets[0].required("revision").unwrap(),
    )
    .await;
    observed(
        &journal,
        "stopped-fixture",
        "stopped",
        targets[1].required("revision").unwrap(),
    )
    .await;
    observed(
        &journal,
        "bootstrap-fixture",
        "ready",
        targets[2].required("revision").unwrap(),
    )
    .await;
    assert!(
        !path(
            service_paths(&accepted, &instances[1])
                .unwrap()
                .required("data")
                .unwrap()
        )
        .join("launches")
        .exists()
    );
    assert_eq!(
        policy_import::import_target_policies(desired.clone(), complete_request)
            .await
            .unwrap_err()
            .code,
        "executor_already_running"
    );
    let mut rollback = Vec::new();
    for (index, id) in [(3, "fixture"), (4, "bootstrap-fixture")] {
        journal
            .restore_target(targets[index].clone())
            .await
            .unwrap();
        let ready = observed(
            &journal,
            id,
            "ready",
            targets[index].required("revision").unwrap(),
        )
        .await;
        assert_eq!(
            ready.required("health").unwrap().get("buildId"),
            Some(&digest("b"))
        );
        if id == "fixture" {
            assert_ne!(
                ready.required("health").unwrap().get("pid"),
                initial.required("health").unwrap().get("pid")
            );
        }
        rollback.push(ready);
    }
    for owner in owners.into_iter().rev() {
        owner.close().await.unwrap();
    }
    for (id, revision) in [
        ("fixture", "rollback-launch"),
        ("bootstrap-fixture", "old-bootstrap-launch"),
    ] {
        let data = path(
            service_paths(
                &accepted,
                instances
                    .iter()
                    .find(|v| v.get("instanceId") == Some(&id.into()))
                    .unwrap(),
            )
            .unwrap()
            .required("data")
            .unwrap(),
        );
        assert_eq!(
            std::fs::read_to_string(data.join("cooperatively-stopped")).unwrap(),
            revision
        );
        assert_eq!(
            std::fs::read_to_string(data.join("retained.txt")).unwrap(),
            "retained application values\n"
        );
    }
    journal.close().await.unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), source_bytes);
    if let Some(file) = std::env::var_os("IVY_MANAGER_POLICY_IMPORT_TRACE") {
        std::fs::write(
            file,
            encoded(&IvyJson::object([
                ("before", before),
                ("plan", plan),
                ("partial", partial),
                ("receipt", receipt),
                ("rollback", IvyJson::Array(rollback)),
                ("sourceCopyPreserved", true.into()),
            ])),
        )
        .unwrap();
    }
}
