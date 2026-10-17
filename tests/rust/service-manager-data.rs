use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_service_manager::topology;
use ivy_service_manager::{
    hints::hint,
    registry,
    reports::{Reports, host_identity},
};

fn json(text: &str) -> IvyJson {
    parse(text, MANAGEMENT_FRAME_BYTES).unwrap()
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
