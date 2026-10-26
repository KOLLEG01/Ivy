#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use ivy_contracts::{
    IvyJson,
    json::{canonical, parse, stringify},
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
async fn mutation_reload_errors_preserve_raw_outcomes_and_admission_order() {
    use ivy_contracts::{IvyError, JsString, Outcome};
    use ivy_sdk::service::InvocationContext;
    use ivy_service_manager::{configuration::Reload, handlers::mutations};
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

    let root = Temp::new();
    for value in [
        r#"["unknown"]"#, r#"["completed"]"#, r#"[["not_executed"]]"#,
        r#""not_executed""#, r#""unknown""#, r#""completed""#,
    ] {
        let raw = json(value);
        for details in [None, Some(IvyJson::Null), Some(IvyJson::object([
            ("key", IvyJson::String(JsString(vec![0xdfff, 65]))),
        ]))] {
            let mut expected = IvyError::new_js(
                "configuration_changed".into(), JsString(vec![0xd800; 2050]),
            ).with_provider_outcome(raw.clone());
            expected.details = details;
            let calls = Arc::new(AtomicUsize::new(0));
            let reload: Reload = Arc::new({
                let calls = calls.clone();
                let expected = expected.clone();
                move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let expected = expected.clone();
                    Box::pin(async move { Err(expected) })
                }
            });
            let handlers = mutations(reload, root.0.clone());
            let context = || InvocationContext::detached(
                "caller".into(), Some("operation".into()), 1, Default::default(),
                IvyError::new("cancelled", "Fixture cancelled."),
            );
            for name in ["host.deploy", "host.rollback", "host.restart", "host.enable", "host.disable"] {
                let before = calls.load(Ordering::SeqCst);
                let handler = &handlers[&JsString::from(name)];
                let error = handler(json(r#"{"instanceId":"instance","operationId":"operation"}"#), context())
                    .await.unwrap_err();
                assert_eq!(error, expected, "{name}: {value}");
                assert_eq!(error.to_wire().get("data").unwrap().get("outcome"), Some(&raw));
                assert_eq!(calls.load(Ordering::SeqCst), before + 1);

                let conflict = handler(json(r#"{"instanceId":"instance","operationId":"other"}"#), context())
                    .await.unwrap_err();
                assert_eq!(conflict.code, "mutation_conflict");
                assert_eq!(conflict.outcome, Outcome::NotExecuted);
                assert_eq!(calls.load(Ordering::SeqCst), before + 1, "Admission must precede reload");
            }
        }
    }
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0, "Failed reload must not open a journal");
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

fn manager_host(root: &Temp) -> IvyJson {
    IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("hostId", "host".into()),
        (
            "runtimeRoot",
            root.0.join("state").to_string_lossy().to_string().into(),
        ),
        (
            "artifactRoot",
            root.0
                .join("artifacts")
                .to_string_lossy()
                .to_string()
                .into(),
        ),
        (
            "stagingRoot",
            root.0.join("staging").to_string_lossy().to_string().into(),
        ),
        ("publicBaseUrl", "http://127.0.0.1:39081/ivy".into()),
        ("executables", json(r#"{"node":"fixture-node"}"#)),
        (
            "instances",
            json(
                r#"[{"instanceId":"fixture","serviceNodeId":"host.fixture","componentId":"fixture","engine":"process","enabled":true,"settings":{}}]"#,
            ),
        ),
    ])
}

#[tokio::test]
async fn journal_handlers_refresh_executor_and_observe_configuration_reload() {
    use ivy_host_runtime::{config::atomic_json, journal::HostJournal};
    use ivy_service_manager::journal::read_handlers;
    let root = Temp::new();
    let mut config = manager_host(&root);
    let journal = HostJournal::open(config.clone()).await.unwrap();
    let handlers = read_handlers(journal.clone(), "host".into());
    let context = || ivy_sdk::service::InvocationContext::detached(
        "caller".into(), None, 1, Default::default(),
        ivy_contracts::IvyError::new("cancelled", "Fixture cancelled."),
    );
    let status = &handlers[&"host.status".into()];
    let first = status(json("{}"), context()).await.unwrap();
    assert_eq!(first.required("executor").unwrap(), &IvyJson::Null);
    let mut executor = json(
        r#"{"schemaVersion":1,"hostId":"host","pid":1,"bootId":"boot","buildId":null,"state":"ready","activeDeploymentId":null,"code":null}"#,
    );
    executor.insert(
        "observedAt",
        chrono::Utc::now()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            .into(),
    );
    let executor_path = root.0.join("state/executor.json");
    atomic_json(executor_path.clone(), executor.clone())
        .await
        .unwrap();
    let fresh = status(json("{}"), context()).await.unwrap();
    assert_eq!(
        canonical(fresh.required("executor").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
        canonical(&executor, MANAGEMENT_FRAME_BYTES).unwrap()
    );
    executor.insert("hostId", "other".into());
    atomic_json(executor_path, executor).await.unwrap();
    let mut instances = config
        .required("instances")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    instances[0].insert("enabled", false.into());
    config.insert("instances", IvyJson::Array(instances));
    journal.use_configuration(config).await.unwrap();
    let reloaded = status(json("{}"), context()).await.unwrap();
    assert_eq!(reloaded.required("executor").unwrap(), &IvyJson::Null);
    assert_eq!(
        reloaded.required("instances").unwrap().as_array().unwrap()[0]
            .required("desiredEnabled")
            .unwrap(),
        &IvyJson::Bool(false)
    );
    let error =
        handlers[&"host.deployment".into()](json(r#"{"deploymentId":"missing"}"#), context())
            .await
            .unwrap_err();
    assert_eq!(error.code, "not_found");
    journal.close().await.unwrap();
}

#[derive(Default)]
struct JournalRpc(Mutex<Vec<(String, IvyJson)>>);
impl RpcClient for JournalRpc {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let mut calls = self.0.lock().unwrap();
            let first_report =
                method == "hosts.report" && !calls.iter().any(|(name, _)| name == method);
            calls.push((method.into(), params));
            if first_report {
                return Err(ivy_contracts::IvyError::new(
                    "outcome_unknown",
                    "lost acknowledgment",
                ));
            }
            Ok(match method {
                "hosts.observations" => json(r#"{"items":[]}"#),
                "hosts.report" | "deployments.report" => IvyJson::Null,
                _ => panic!("Unexpected RPC {method}"),
            })
        })
    }
}

#[tokio::test]
async fn journal_publication_keeps_durable_sequences_across_failed_ack_and_reopen() {
    use ivy_host_runtime::journal::HostJournal;
    let root = Temp::new();
    let config = manager_host(&root);
    let journal = HostJournal::open(config.clone()).await.unwrap();
    let rpc = JournalRpc::default();
    let mut reports = Reports::default();
    reports.reconcile_from(&rpc, "host".into()).await.unwrap();
    assert!(
        reports
            .publish_journal(&rpc, &journal, &"host".into())
            .await
            .is_err()
    );
    assert_eq!(reports.minimum_sequence(), 0);
    assert_eq!(rpc.0.lock().unwrap().len(), 2); // No deployments after failed host report.
    assert!(
        reports
            .publish_journal(&rpc, &journal, &"host".into())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(reports.minimum_sequence(), 2);
    reports
        .publish_journal(&rpc, &journal, &"host".into())
        .await
        .unwrap();
    assert_eq!(rpc.0.lock().unwrap().len(), 4); // Timestamp pulses do not republish.
    journal.close().await.unwrap();
    let journal = HostJournal::open(config).await.unwrap();
    let mut next = Reports::default();
    next.reconcile_from(&rpc, "host".into()).await.unwrap();
    next.publish_journal(&rpc, &journal, &"host".into())
        .await
        .unwrap();
    assert_eq!(next.minimum_sequence(), 3);
    let calls = rpc.0.lock().unwrap();
    let sequences: Vec<_> = calls
        .iter()
        .filter(|(name, _)| name == "hosts.report")
        .map(|(_, value)| value.required("sequence").unwrap().as_f64().unwrap())
        .collect();
    assert_eq!(sequences, vec![1.0, 2.0, 3.0]);
    for (_, value) in calls
        .iter()
        .filter(|(name, _)| name == "deployments.report")
    {
        assert_eq!(value, &json(r#"{"records":[]}"#));
    }
    drop(calls);
    journal.close().await.unwrap();
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
        std::fs::read_to_string(path).unwrap(),
        format!(
            "{}\n",
            canonical(
                &hint(&batch, &configuration, &catalog, "now")
                    .unwrap()
                    .unwrap(),
                MANAGEMENT_FRAME_BYTES,
            )
            .unwrap()
        )
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
    let late_ready = async { reporter.write(true, Some(7), "late readiness".into()).await };
    reporter.finalize().await.unwrap();
    assert_eq!(late_ready.await.unwrap_err().code, "service_unavailable");
    reporter.flush().await.unwrap();
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
    let output = reference_data::read(include_bytes!("../fixtures/service-manager-registry-cases.json.gz"));
    let expected = json(std::str::from_utf8(&output).unwrap());
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
fn topology_owns_hive_and_disabled_services_without_restarting_existing_owners() {
    let accepted = json(
        r#"{"instances":[{"instanceId":"first","componentId":"hive","engine":"process","enabled":true},{"instanceId":"new","componentId":"data-collector","engine":"process","enabled":false},{"instanceId":"host","componentId":"host-executor","engine":"process","enabled":true},{"instanceId":"manager","componentId":"service-manager","engine":"process","enabled":true},{"instanceId":"external","componentId":"sample","engine":"docker","enabled":true}]}"#,
    );
    let desired = topology::owner_ids("win32", &accepted).unwrap();
    assert_eq!(desired, vec!["first".into(), "new".into()]);
    let changes = topology::changes(&["first".into(), "old-a".into(), "old-b".into()], &desired);
    assert_eq!(changes.added, vec!["new".into()]);
    assert_eq!(changes.removed, vec!["old-b".into(), "old-a".into()]);
    assert!(topology::owner_ids("linux", &accepted).unwrap().is_empty());
}
