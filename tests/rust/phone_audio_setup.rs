use super::*;
use ivy_contracts::json::parse;

fn value(text: &str) -> IvyJson {
    parse(text, 65536).unwrap()
}
fn inventory() -> IvyJson {
    value(
        r#"{"processLoopbackSupported":true,"devices":[{"id":"capture","name":"Capture","direction":"capture","state":"Active","muted":false},{"id":"receive","name":"Receive","direction":"render","state":"Active","muted":false},{"id":"speaker","name":"Speaker","direction":"render","state":"Active","muted":true}]}"#,
    )
}
fn profiles() -> IvyJson {
    IvyJson::Array(["windows"].into_iter().map(|route|IvyJson::object([
        ("route",route.into()),("settings",value(r#"{"captureEndpointId":"capture","renderEndpointId":"receive","sourceMode":"desktop_process","mutePolicy":{"mutedRenderEndpointId":"speaker","unmuteReceive":true}}"#)),
    ])).collect())
}
fn routes(report: &IvyJson) -> &[IvyJson] {
    report.get("routes").unwrap().as_array().unwrap()
}
fn field_mut<'a>(value: &'a mut IvyJson, key: &str) -> &'a mut IvyJson {
    let IvyJson::Object(fields) = value else {
        panic!("Expected a fixture object")
    };
    &mut fields
        .iter_mut()
        .find(|(name, _)| name == &ivy_contracts::JsString::from(key))
        .unwrap()
        .1
}
#[test]
fn exact_endpoint_readiness_is_read_only_and_refuses_ambiguous_or_conflicting_routes() {
    let inventory = inventory();
    let before = inventory.clone();
    let profiles = profiles();
    let report = setup(&inventory, &profiles).unwrap();
    crate::contracts::validate_service("PhoneAudioSetup", &report).unwrap();
    assert!(
        routes(&report)
            .iter()
            .all(|r| r.get("ready") == Some(&true.into()))
    );
    assert_eq!(inventory, before);
    for (field, change, state) in [
        ("muted", false.into(), "mute_mismatch"),
        ("muted", IvyJson::Null, "mute_unknown"),
        ("state", "Disabled".into(), "inactive"),
        ("direction", "capture".into(), "wrong_direction"),
        ("id", "foreign".into(), "missing"),
    ] {
        let mut changed = inventory.clone();
        if let IvyJson::Array(devices) = field_mut(&mut changed, "devices") {
            devices[2].insert(field, change);
        }
        let report = setup(&changed, &profiles).unwrap();
        crate::contracts::validate_service("PhoneAudioSetup", &report).unwrap();
        assert_eq!(routes(&report)[0].get("ready"), Some(&false.into()));
        assert_eq!(
            routes(&report)[0]
                .get("endpoints")
                .unwrap()
                .as_array()
                .unwrap()[1]
                .get("state"),
            Some(&state.into())
        );
    }
    let mut duplicate = inventory.clone();
    if let IvyJson::Array(devices) = field_mut(&mut duplicate, "devices") {
        devices.push(devices[2].clone());
    }
    assert_eq!(
        routes(&setup(&duplicate, &profiles).unwrap())[0]
            .get("endpoints")
            .unwrap()
            .as_array()
            .unwrap()[1]
            .get("state"),
        Some(&"ambiguous".into())
    );
    let mut unsupported = inventory.clone();
    unsupported.insert("processLoopbackSupported", false.into());
    assert!(
        routes(&setup(&unsupported, &profiles).unwrap())
            .iter()
            .all(|r| r.get("ready") == Some(&false.into()))
    );
    let mut conflicting = profiles.clone();
    if let IvyJson::Array(rows) = &mut conflicting {
        for row in rows {
            field_mut(row, "settings").insert(
                "mutePolicy",
                value(r#"{"mutedRenderEndpointId":"receive","unmuteReceive":false}"#),
            );
        }
    }
    assert!(
        routes(&setup(&inventory, &conflicting).unwrap())
            .iter()
            .all(|r| r.get("ready") == Some(&false.into())
                && r.get("mutePolicyConflict") == Some(&true.into()))
    );
}
#[test]
fn microphone_and_virtual_speaker_check_actual_directions_without_process_loopback() {
    let mut inventory = inventory();
    inventory.insert("processLoopbackSupported", false.into());
    for (mode, role) in [("microphone", "capture"), ("virtual_speaker", "loopback")] {
        let mut settings = value(
            r#"{"captureEndpointId":"capture","renderEndpointId":"receive","loopbackRenderEndpointId":"speaker"}"#,
        );
        settings.insert("sourceMode", mode.into());
        let profiles = IvyJson::Array(vec![IvyJson::object([
            ("route", "windows".into()),
            ("settings", settings),
        ])]);
        let report = setup(&inventory, &profiles).unwrap();
        let route = &routes(&report)[0];
        assert_eq!(route.get("ready"), Some(&true.into()));
        let endpoints = route.get("endpoints").unwrap().as_array().unwrap();
        assert_eq!(endpoints[0].get("role"), Some(&role.into()));
        assert_eq!(endpoints[1].get("role"), Some(&"receive".into()));
        if mode == "virtual_speaker" {
            let mut conflict = profiles;
            if let IvyJson::Array(rows) = &mut conflict {
                field_mut(&mut rows[0], "settings")
                    .insert("loopbackRenderEndpointId", "receive".into());
            }
            assert_eq!(
                routes(&setup(&inventory, &conflict).unwrap())[0].get("ready"),
                Some(&false.into())
            );
        }
    }
}
