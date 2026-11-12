use super::*;

#[test]
fn configuration_revisions_project_only_requested_hosts_and_keep_lossless_ids() {
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    store.connection().execute("INSERT INTO principals VALUES('fixture')", []).unwrap();
    let lone = JsString(vec![0xd800]);
    for (key, revision) in [(JsString::from("shown"), Some(7)), (lone.clone(), None), (JsString::from("other"), Some(9))] {
        let id = text(&key);
        store.connection().execute("INSERT INTO service_nodes VALUES(?1,'fixture','host','service',1,'{}')", [&id]).unwrap();
        let executor = revision.map(|revision| IvyJson::object([("configurationRevision", (revision as f64).into())])).unwrap_or(IvyJson::Null);
        let snapshot = IvyJson::object([("status", IvyJson::object([
            ("hostId", IvyJson::String(key)), ("executor", executor), ("unneeded", "x".repeat(512 * 1024).into()),
        ]))]);
        store.connection().execute("INSERT INTO host_reports VALUES(?1,?1,1,'1970-01-01T00:00:00.000Z',?2)", params![id, canonical(&snapshot, MANAGEMENT_FRAME_BYTES).unwrap()]).unwrap();
    }
    let result = Registry::default().configuration_revisions(&store, &IvyJson::object([
        ("hostIds", IvyJson::Array(vec!["shown".into(), IvyJson::String(lone.clone()), "absent".into()])),
    ])).unwrap();
    let items = result.required("items").unwrap().as_array().unwrap();
    assert_eq!(items.len(), 2);
    let shown = items.iter().find(|row| row.get("hostId") == Some(&"shown".into())).unwrap();
    assert_eq!(shown.required("configurationRevision").unwrap(), &IvyJson::Number(7.0));
    let lone = IvyJson::String(lone);
    let no_revision = items.iter().find(|row| row.get("hostId") == Some(&lone)).unwrap();
    assert!(no_revision.required("configurationRevision").unwrap().is_null());
    assert!(items.iter().all(|row| row.as_object().unwrap().len() == 2));
}
