use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_service_manager::topology;
use ivy_service_manager::{
    hints::hint,
    registry,
    reports::{Reports, host_identity},
};
use std::{collections::VecDeque, sync::Mutex};

fn json(text: &str) -> IvyJson {
    parse(text, MANAGEMENT_FRAME_BYTES).unwrap()
}

struct ReplyQueue(Mutex<VecDeque<(&'static str, IvyJson, ivy_contracts::IvyResult<IvyJson>)>>);
impl RpcClient for ReplyQueue {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let (expected, input, reply) =
                self.0.lock().unwrap().pop_front().expect("Unexpected RPC");
            assert_eq!(method, expected);
            assert_eq!(params, input);
            reply
        })
    }
}

#[tokio::test]
async fn publication_retries_failed_acks_without_reallocating_unchanged_reports() {
    let status = json(r#"{"hostId":"host","executor":null,"instances":[]}"#);
    let snapshot = json(r#"{"sequence":12}"#);
    let unknown = || ivy_contracts::IvyError::new("outcome_unknown", "fixture");
    let rpc = ReplyQueue(Mutex::new(VecDeque::from([
        (
            "hosts.observations",
            json(r#"{"hostId":"host"}"#),
            Ok(IvyJson::object([(
                "items",
                IvyJson::Array(vec![IvyJson::object([(
                    "snapshot",
                    IvyJson::object([
                        ("sequence", IvyJson::Number(11.0)),
                        ("status", status.clone()),
                    ]),
                )])]),
            )])),
        ),
        ("hosts.report", snapshot.clone(), Err(unknown())),
        ("hosts.report", snapshot.clone(), Ok(IvyJson::Null)),
        (
            "deployments.report",
            json(r#"{"records":[]}"#),
            Err(unknown()),
        ),
        (
            "deployments.report",
            json(r#"{"records":[]}"#),
            Ok(IvyJson::Null),
        ),
    ])));
    let mut reports = Reports::default();
    reports.reconcile_from(&rpc, "host".into()).await.unwrap();
    assert_eq!(reports.minimum_sequence(), 11);
    reports
        .publish_host(&rpc, &status, |_| async {
            panic!("Unchanged status allocated a sequence")
        })
        .await
        .unwrap();
    let mut changed = status.clone();
    changed.insert("retention", IvyJson::Null);
    assert!(
        reports
            .publish_host(&rpc, &changed, |minimum| {
                assert_eq!(minimum, 11);
                let snapshot = snapshot.clone();
                async { Ok(snapshot) }
            })
            .await
            .is_err()
    );
    assert_eq!(reports.minimum_sequence(), 11);
    reports
        .publish_host(&rpc, &changed, |_| async { Ok(snapshot.clone()) })
        .await
        .unwrap();
    assert_eq!(reports.minimum_sequence(), 12);
    reports
        .publish_host(&rpc, &changed, |_| async {
            panic!("Acknowledged status allocated again")
        })
        .await
        .unwrap();
    assert!(reports.publish_deployments(&rpc, json("[]")).await.is_err());
    reports.publish_deployments(&rpc, json("[]")).await.unwrap();
    reports.publish_deployments(&rpc, json("[]")).await.unwrap();
    assert!(rpc.0.lock().unwrap().is_empty());
}

struct Temp(std::path::PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ivy-manager-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn desired_subscription_keeps_order_and_persistence_failures_propagate() {
    use ivy_service_manager::hints::{desired_subscription, persist_hint};
    let rpc = ReplyQueue(Mutex::new(VecDeque::from([
        (
            "packages.catalog",
            json(r#"{"after":0,"includeUis":true}"#),
            Ok(json(r#"{"objectId":"catalog"}"#)),
        ),
        (
            "hostConfigurations.get",
            json(r#"{"hostId":"host"}"#),
            Ok(json(r#"{"objectId":"config"}"#)),
        ),
    ])));
    let (subscription, configuration, catalog) =
        desired_subscription(&rpc, "host".into()).await.unwrap();
    let digest = ivy_contracts::hash::digest("config:catalog");
    assert_eq!(
        subscription.required("name").unwrap().as_str().unwrap(),
        format!("host-desired-{}", &digest[7..31])
    );
    assert_eq!(
        subscription.required("filter").unwrap(),
        &json(r#"{"topics":["hive.object.changed"],"objectIds":["config","catalog"]}"#)
    );
    let root = Temp::new();
    let path = root.0.join("desired-state-hint.json");
    let empty = json(r#"{"items":[],"gap":null,"throughSequence":4}"#);
    persist_hint(path.clone(), &empty, &configuration, &catalog, "now")
        .await
        .unwrap();
    assert!(!path.exists());
    let batch = json(r#"{"items":[],"gap":{"prunedThroughSequence":3},"throughSequence":4}"#);
    persist_hint(path.clone(), &batch, &configuration, &catalog, "now")
        .await
        .unwrap();
    assert_eq!(
        json(&std::fs::read_to_string(path).unwrap()),
        hint(&batch, &configuration, &catalog, "now")
            .unwrap()
            .unwrap()
    );
    assert!(
        persist_hint(root.0.clone(), &batch, &configuration, &catalog, "now")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn health_queue_survives_cancelled_waiters_and_rejects_stale_executor() {
    use ivy_host_runtime::{config::InstanceConfig, health::HealthFile};
    use ivy_service_manager::health::{HealthReporter, current_executor};
    let root = Temp::new();
    let config = InstanceConfig::from_json(IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("hostId", "host".into()),
        ("instanceId", "manager".into()),
        ("serviceNodeId", "host.manager".into()),
        ("componentId", "service-manager".into()),
        ("publicBaseUrl", "https://example.invalid/ivy".into()),
        ("dataRoot", root.0.to_string_lossy().to_string().into()),
        (
            "artifactRoot",
            root.0.join("artifact").to_string_lossy().to_string().into(),
        ),
        ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
        ("version", "1.0.0".into()),
        ("credential", "fixture".into()),
        ("settings", json("{}")),
    ]))
    .unwrap();
    let reporter = HealthReporter::new(std::sync::Arc::new(HealthFile::with_launch_id(
        config,
        Some("launch".into()),
    )));
    let executor = json(
        r#"{"schemaVersion":1,"hostId":"host","pid":1,"bootId":"boot","buildId":null,"observedAt":"2026-01-01T00:00:00.000Z","state":"ready","activeDeploymentId":null,"code":null}"#,
    );
    let at = ivy_sdk::clock::parse_date(&"2026-01-01T00:00:00.000Z".into()) as i64;
    for (delta, accepted) in [(-1001, false), (-1000, true), (14999, true), (15000, false)] {
        assert_eq!(
            current_executor(executor.clone(), &"host".into(), at + delta).is_some(),
            accepted
        );
    }
    assert!(current_executor(executor.clone(), &"other".into(), at).is_none());
    let mut malformed = executor.clone();
    malformed.insert("pid", IvyJson::Number(0.0));
    assert!(current_executor(malformed, &"host".into(), at).is_none());
    let ready = reporter
        .readiness(Some(&executor), "host".into(), "host.manager".into(), 7, at)
        .await
        .unwrap();
    assert_eq!(ready.required("ready").unwrap(), &IvyJson::Bool(true));
    // All writes are accepted synchronously even when callers never poll their futures.
    drop(reporter.write(true, Some(7), "cancelled waiter".into()));
    reporter.state(ivy_sdk::service::ServiceState {
        status: "offline",
        generation: Some(7),
        code: Some("lost".into()),
        message: None,
        phase: None,
    });
    reporter
        .write(false, None, "ServiceManager stopped.".into())
        .await
        .unwrap();
    let health = json(&std::fs::read_to_string(root.0.join("health.json")).unwrap());
    assert_eq!(health.required("ready").unwrap(), &IvyJson::Bool(false));
    assert_eq!(health.required("generation").unwrap(), &IvyJson::Null);
    assert_eq!(
        health.required("details").unwrap(),
        &IvyJson::from("ServiceManager stopped.")
    );
}

#[test]
fn host_report_ignores_only_observation_timestamps() {
    let before = json(
        r#"{"hostId":"host","observedAt":"first","executor":{"observedAt":"first","state":"ready"},"instances":[{"instanceId":"a","observedAt":"first","ready":true}],"activeDeployments":[],"retention":{"observedAt":"retained"}}"#,
    );
    let after = json(
        r#"{"hostId":"host","observedAt":"second","executor":{"observedAt":"second","state":"ready"},"instances":[{"instanceId":"a","observedAt":"second","ready":true}],"activeDeployments":[],"retention":{"observedAt":"retained"}}"#,
    );
    assert_eq!(
        host_identity(&before).unwrap(),
        host_identity(&after).unwrap()
    );
    let changed = json(
        r#"{"hostId":"host","observedAt":"second","executor":{"observedAt":"second","state":"ready"},"instances":[{"instanceId":"a","observedAt":"second","ready":false}],"activeDeployments":[],"retention":{"observedAt":"retained"}}"#,
    );
    assert_ne!(
        host_identity(&before).unwrap(),
        host_identity(&changed).unwrap()
    );
    assert!(
        host_identity(&before)
            .unwrap()
            .contains(r#""retention":{"observedAt":"retained"}"#)
    );
}

#[test]
fn identity_preserves_utf16_values_optional_fields_and_js_property_order() {
    let status = json(
        r#"{"observedAt":"now","instances":[{"name":"\ud800","observedAt":"now"}],"hostId":"host","executor":null,"optional":null,"10":"ten","2":"two"}"#,
    );
    assert_eq!(
        host_identity(&status).unwrap(),
        r#"{"2":"two","10":"ten","hostId":"host","optional":null,"executor":null,"instances":[{"name":"\ud800","observedAt":null}]}"#
    );
}

#[test]
fn report_cache_advances_only_after_ack_and_resets_deployments_on_reconnect() {
    let status = json(r#"{"observedAt":"now","hostId":"host","executor":null,"instances":[]}"#);
    let identity = host_identity(&status).unwrap();
    let mut reports = Reports::default();
    assert!(reports.host_changed(&identity));
    // Constructing/sending a report is not an acknowledgement.
    assert!(reports.host_changed(&identity));
    assert_eq!(reports.minimum_sequence(), 0);
    reports.host_acknowledged(4, identity.clone());
    reports.deployments_acknowledged("[]".into());
    assert!(!reports.host_changed(&identity));
    assert!(!reports.deployments_changed("[]"));
    reports
        .reconcile(&[(3, status.clone()), (9, status.clone())])
        .unwrap();
    assert_eq!(reports.minimum_sequence(), 9);
    assert!(!reports.host_changed(&identity));
    assert!(reports.deployments_changed("[]"));
    reports.reconcile(&[]).unwrap();
    assert!(reports.host_changed(&identity));
    assert_eq!(reports.minimum_sequence(), 9);
}

#[test]
fn desired_batches_keep_the_existing_hint_shape() {
    let configuration = IvyJson::from("config");
    let catalog = IvyJson::from("catalog");
    for (batch, expected) in [
        (r#"{"gap":null,"items":[],"throughSequence":7}"#, None),
        (
            r#"{"gap":{"firstAvailableSequence":4},"items":[],"throughSequence":7}"#,
            Some((true, true)),
        ),
        (
            r#"{"gap":null,"items":[{"payload":{"objectId":"config"}}],"throughSequence":7}"#,
            Some((true, false)),
        ),
        (
            r#"{"gap":null,"items":[{"payload":{"objectId":"catalog"}}],"throughSequence":7}"#,
            Some((false, true)),
        ),
        (
            r#"{"gap":null,"items":[{"payload":{"objectId":"config"}},{"payload":{"objectId":"catalog"}}],"throughSequence":7}"#,
            Some((true, true)),
        ),
        (
            r#"{"gap":null,"items":[{"payload":{}}],"throughSequence":7}"#,
            Some((false, false)),
        ),
    ] {
        let value = hint(&json(batch), &configuration, &catalog, "now").unwrap();
        match expected {
            None => assert!(value.is_none()),
            Some((config, packages)) => {
                let expected = format!(
                    r#"{{"schemaVersion":1,"throughSequence":7,"observedAt":"now","configuration":{config},"packages":{packages}}}"#
                );
                assert_eq!(
                    stringify(&value.unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
                    expected
                );
            }
        }
    }
}

#[test]
fn registry_requests_the_normative_host_and_wire_schema_roots() {
    let mut calls = vec![];
    let registry = registry::registry(|reference, document| {
        calls.push((reference.to_string(), document.to_string()));
        Ok(IvyJson::from(reference))
    })
    .unwrap();
    assert_eq!(calls.len(), 14);
    assert_eq!(
        calls[0],
        (
            "#/$defs/ManagerStatusInput".into(),
            registry::HOST_SCHEMA.into()
        )
    );
    assert_eq!(
        calls[1],
        ("#/$defs/HostStatus".into(), registry::HOST_SCHEMA.into())
    );
    assert_eq!(
        calls[3],
        ("#/$defs/JournalEntry".into(), registry::HOST_SCHEMA.into())
    );
    for output in [5, 7, 9, 11, 13] {
        assert_eq!(
            calls[output],
            (
                "#/$defs/DeploymentRecord".into(),
                registry::WIRE_SCHEMA.into()
            )
        );
    }
    let IvyJson::Array(namespaces) = registry.get("namespaces").unwrap() else {
        panic!()
    };
    let IvyJson::Array(tools) = namespaces[0].get("tools").unwrap() else {
        panic!()
    };
    for (i, name) in [
        "status",
        "deployment",
        "deploy",
        "rollback",
        "restart",
        "enable",
        "disable",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(tools[i].get("name"), Some(&IvyJson::from(*name)));
        assert_eq!(
            tools[i].get("annotations").unwrap().get("readOnlyHint"),
            Some(&IvyJson::Bool(i < 2))
        );
    }
}

#[test]
fn bundled_tool_definitions_match_original_manager_exactly() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/service-manager-registry-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = json(std::str::from_utf8(&output.stdout).unwrap());
    let registry = registry::bundled_registry().unwrap();
    let actual = registry.required("namespaces").unwrap().as_array().unwrap()[0]
        .required("tools")
        .unwrap();
    assert_eq!(
        stringify(actual, MANAGEMENT_FRAME_BYTES).unwrap(),
        stringify(&expected, MANAGEMENT_FRAME_BYTES).unwrap()
    );
}

#[test]
fn topology_adds_disabled_instances_without_restarting_existing_owners() {
    let accepted = json(
        r#"{"instances":[{"instanceId":"first","componentId":"sample","engine":"process","enabled":true},{"instanceId":"new","componentId":"sample","engine":"process","enabled":false},{"instanceId":"manager","componentId":"service-manager","engine":"process","enabled":true},{"instanceId":"external","componentId":"sample","engine":"docker","enabled":true}]}"#,
    );
    let desired = topology::owner_ids("win32", &accepted).unwrap();
    assert_eq!(desired, vec!["first".into(), "new".into()]);
    let changes = topology::changes(&["first".into(), "old-a".into(), "old-b".into()], &desired);
    assert_eq!(changes.added, vec!["new".into()]);
    assert_eq!(changes.removed, vec!["old-b".into(), "old-a".into()]);
    assert!(topology::owner_ids("linux", &accepted).unwrap().is_empty());
}
