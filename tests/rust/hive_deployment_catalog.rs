use super::*;
use crate::{
    native_runtime::NativeRuntime,
    retention::graph_tests::{LIVE, PEAK},
};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, atomic::Ordering},
};

fn fixture(run: impl FnOnce(&mut Call<'_>)) {
    let mut store = Store::open(Path::new(":memory:")).unwrap();
    store.connection().execute_batch(r#"
        INSERT INTO principals VALUES('fixture');
        INSERT INTO service_nodes VALUES('fixture','fixture','host','service',1,
          '{"serviceNodeId":"fixture","connected":true,"lastContactAt":"1970-01-01T00:00:00.000Z"}');
        INSERT INTO service_nodes VALUES('offline','fixture','other','service',1,
          '{"serviceNodeId":"offline","connected":false}');
    "#).unwrap();
    let runtime = NativeRuntime;
    store.bind_query_runtime(Arc::new(runtime));
    let mut registry = Registry::default();
    let mut discovery = crate::discovery::Discovery::new(&runtime).unwrap();
    let configurations = crate::configurations::Configurations::new(&mut store).unwrap();
    let options = Options {
        version: "test".into(),
        build_id: "test".into(),
        public_base_url: "test".into(),
        validate_messages: false,
        reset_bootstrap_credential_digest: None,
    };
    let files = crate::ui_files::UiFiles::new(Path::new("unused-deployment-fixture"));
    let handlers = HashMap::new();
    let mut generation = 0.0;
    run(&mut Call {
        store: &mut store,
        registry: &mut registry,
        discovery: &mut discovery,
        configurations: &configurations,
        ui_files: &files,
        options: &options,
        runtime: &runtime,
        clock: &|| 0.0,
        time_zones: None,
        connection_generation: &mut generation,
        handlers: &handlers,
    });
}
fn json(value: serde_json::Value) -> IvyJson {
    IvyJson::from_serde(&value).unwrap()
}
fn insert(call: &Call<'_>, host: &str, deployment: &str, instance: &str, date: &str, bytes: usize) {
    let record = json(
        serde_json::json!({"hostId":host,"deploymentId":deployment,"instanceId":instance,
        "updatedAt":date,"opaque": "x".repeat(bytes)}),
    );
    let encoded = canonical(&record, MANAGEMENT_FRAME_BYTES).unwrap();
    call.store
        .connection()
        .execute(
            "INSERT INTO deployment_reports VALUES(?,?,?,?,?)",
            params![
                text(string(&record, "hostId").unwrap()),
                text(string(&record, "deploymentId").unwrap()),
                if host == "other" {
                    "offline"
                } else {
                    "fixture"
                },
                "1970-01-01T00:00:00.000Z",
                encoded
            ],
        )
        .unwrap();
}
fn reference(call: &Call<'_>, params: &IvyJson) -> IvyJson {
    let mut statement = call
        .store
        .connection()
        .prepare("SELECT host_id,deployment_id FROM deployment_reports")
        .unwrap();
    let ids = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let records = ids
        .into_iter()
        .map(|(host, id)| read(call, &host, &id).unwrap().unwrap())
        .filter(|value| {
            let record = value.get("record").unwrap();
            selected(params, "hostId", record.get("hostId"))
                && selected(params, "instanceId", record.get("instanceId"))
        })
        .collect();
    try_page(
        call,
        "deployments.list",
        params,
        records,
        |value| {
            let record = value.get("record").unwrap();
            let date = call.runtime.parse_date(string(record, "updatedAt")?);
            let mut newest: JsString = (9_007_199_254_740_991.0 - date).to_string().into();
            if newest.0.len() < 16 {
                let mut prefix = vec![48; 16 - newest.0.len()];
                prefix.extend(newest.0);
                newest = JsString(prefix);
            }
            Ok(canonical(
                &IvyJson::Array(vec![
                    newest.into(),
                    record.get("hostId").cloned().unwrap_or(IvyJson::Null),
                    record.get("deploymentId").cloned().unwrap_or(IvyJson::Null),
                ]),
                MANAGEMENT_FRAME_BYTES,
            )?
            .into())
        },
        None,
    )
    .unwrap()
}
fn items(page: &IvyJson) -> &[IvyJson] {
    page.get("items").unwrap().as_array().unwrap()
}

#[test]
fn deployment_projection_preserves_order_filters_unicode_and_old_cursors() {
    fixture(|call| {
        for (host, id, instance, date) in [
            ("host", "same-a", "alpha", "1970-01-01T00:00:00.000Z"),
            ("host", "same-b", "beta", "1970-01-01T00:00:00.000Z"),
            ("other", "new", "alpha", "2026-10-29T20:00:00.000Z"),
            ("host", "é", "alpha", "1960-01-01T00:00:00.000Z"),
            ("host", "😀", "alpha", "1960-01-01T00:00:00.000Z"),
            ("host", "minimum", "alpha", "0000-01-01T00:00:00.000Z"),
            ("host", "maximum", "alpha", "9999-12-31T23:59:59.999Z"),
            ("host", "safe-integer", "alpha", "1969-12-31T23:59:59.999Z"),
            (
                "host",
                "rounded-integer",
                "alpha",
                "1969-12-31T23:59:59.998Z",
            ),
            (
                "host",
                "quote\"\\\n",
                "quote\"\\\n",
                "0001-01-01T00:00:00.000Z",
            ),
        ] {
            insert(call, host, id, instance, date, 0);
        }
        for params in [
            serde_json::json!({"limit":1}),
            serde_json::json!({"limit":2,"hostId":"host"}),
            serde_json::json!({"limit":1,"instanceId":"alpha"}),
            serde_json::json!({"limit":10,"hostId":"","instanceId":""}),
            serde_json::json!({"limit":10,"hostId":"absent"}),
            serde_json::json!({"instanceId":"quote\"\\\n"}),
        ] {
            let params = json(params);
            let mut query = params.clone();
            let mut count = 0;
            loop {
                let expected = reference(call, &query);
                let actual = list(call, &query).unwrap();
                assert_eq!(actual, expected);
                for value in items(&actual) {
                    let record = value.get("record").unwrap();
                    let found = get(
                        call,
                        &IvyJson::object([
                            ("hostId", record.get("hostId").unwrap().clone()),
                            ("deploymentId", record.get("deploymentId").unwrap().clone()),
                        ]),
                    )
                    .unwrap();
                    assert_eq!(&found, value);
                }
                let next = actual.get("nextCursor").unwrap();
                if next.is_null() {
                    break;
                }
                query = params.clone();
                query.insert("cursor", next.clone());
                count += 1;
                assert!(count < 20);
            }
        }
        assert_eq!(
            get(
                call,
                &json(serde_json::json!({"hostId":"host","deploymentId":"absent"}))
            )
            .unwrap_err()
            .code,
            "not_found"
        );
        assert_eq!(
            list(call, &json(serde_json::json!({"cursor":"invalid"})))
                .unwrap_err()
                .code,
            "invalid_cursor"
        );
        let first = reference(call, &json(serde_json::json!({"limit":1,"hostId":"host"})));
        let cursor = first.get("nextCursor").unwrap().clone();
        assert_eq!(
            list(
                call,
                &IvyJson::object([
                    ("limit", 1.0.into()),
                    ("hostId", "other".into()),
                    ("cursor", cursor)
                ])
            )
            .unwrap_err()
            .code,
            "invalid_cursor"
        );
        // SQL identity replacement cannot make a different original unit match.
        let mut record = json(
            serde_json::json!({"hostId":"host","deploymentId":"replacement","instanceId":"unit","updatedAt":"1970-01-01T00:00:00.000Z"}),
        );
        record.insert("deploymentId", JsString(vec![0xd800]).into());
        call.store
            .connection()
            .execute(
                "INSERT INTO deployment_reports VALUES('host',?,'fixture','now',?)",
                params![
                    "\u{fffd}",
                    canonical(&record, MANAGEMENT_FRAME_BYTES).unwrap()
                ],
            )
            .unwrap();
        assert!(
            get(
                call,
                &IvyJson::object([
                    ("hostId", "host".into()),
                    ("deploymentId", JsString(vec![0xd800]).into())
                ])
            )
            .is_ok()
        );
        assert_eq!(
            get(
                call,
                &json(serde_json::json!({"hostId":"host","deploymentId":"�"}))
            )
            .unwrap_err()
            .code,
            "not_found"
        );
        let query = json(serde_json::json!({"instanceId":"unit"}));
        assert_eq!(list(call, &query).unwrap(), reference(call, &query));
        call.store
            .connection()
            .execute(
                "UPDATE deployment_reports SET record_json='[' WHERE deployment_id='same-a'",
                [],
            )
            .unwrap();
        assert_eq!(
            get(
                call,
                &json(serde_json::json!({"hostId":"host","deploymentId":"same-a"}))
            )
            .unwrap_err()
            .code,
            "internal_error"
        );
        assert_eq!(
            get(
                call,
                &json(serde_json::json!({"hostId":"host","deploymentId":"absent"}))
            )
            .unwrap_err()
            .code,
            "not_found"
        );
        assert_eq!(
            list(call, &json(serde_json::json!({"cursor":"invalid"})))
                .unwrap_err()
                .code,
            "invalid_cursor"
        );
        assert_eq!(
            list(call, &json(serde_json::json!({}))).unwrap_err().code,
            "internal_error"
        );
    });
}

#[test]
fn deployment_page_byte_boundary_preserves_last_item_and_large_entry_error() {
    fixture(|call| {
        for id in ["a", "b", "c"] {
            insert(
                call,
                "host",
                id,
                "large",
                "1970-01-01T00:00:00.000Z",
                1_100_000,
            );
        }
        let params = json(serde_json::json!({"limit":100,"instanceId":"large"}));
        let mut query = params.clone();
        for id in ["a", "b", "c"] {
            let expected = reference(call, &query);
            let page = list(call, &query).unwrap();
            assert_eq!(page, expected);
            assert_eq!(items(&page).len(), 1);
            assert_eq!(
                items(&page)[0].get("record").unwrap().get("deploymentId"),
                Some(&id.into())
            );
            let cursor = page.get("nextCursor").unwrap();
            if id == "c" {
                assert!(cursor.is_null());
            } else {
                query = params.clone();
                query.insert("cursor", cursor.clone());
            }
        }
        insert(
            call,
            "host",
            "huge",
            "huge",
            "1970-01-01T00:00:00.000Z",
            crate::paging::PAGE_BYTES,
        );
        assert_eq!(
            list(call, &json(serde_json::json!({"instanceId":"huge"})))
                .unwrap_err()
                .code,
            "result_too_large"
        );
    });
}

#[test]
fn deployment_page_keeps_unselected_report_bodies_out_of_rust_working_memory() {
    const CHILD: &str = "IVY_DEPLOYMENT_PAGE_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output=std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","kernel::catalogs::deployments::tests::deployment_page_keeps_unselected_report_bodies_out_of_rust_working_memory","--nocapture"])
            .env(CHILD,"1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
        return;
    }
    fixture(|call| {
        for index in 0..48 {
            insert(
                call,
                "host",
                &format!("id{index:03}"),
                "fixture",
                "1970-01-01T00:00:00.000Z",
                64 * 1024,
            );
        }
        let params = json(serde_json::json!({"limit":1}));
        let before = LIVE.load(Ordering::Relaxed);
        PEAK.store(before, Ordering::Relaxed);
        let expected = reference(call, &params);
        let full_peak = PEAK.load(Ordering::Relaxed) - before;
        let before = LIVE.load(Ordering::Relaxed);
        PEAK.store(before, Ordering::Relaxed);
        let actual = list(call, &params).unwrap();
        let selected_peak = PEAK.load(Ordering::Relaxed) - before;
        assert_eq!(actual, expected);
        assert_eq!(items(&actual).len(), 1);
        eprintln!(
            "Hive deployment page Rust allocation peak: full reports {full_peak} bytes, selected page {selected_peak} bytes for 48 reports with 3 MiB opaque content"
        );
        assert!(full_peak > 6 * 1024 * 1024);
        assert!(selected_peak < 1024 * 1024);
    });
}

#[test]
fn deployment_keyset_limits_rust_working_memory_for_large_identity_sets() {
    const CHILD: &str = "IVY_DEPLOYMENT_KEYSET_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "kernel::catalogs::deployments::tests::deployment_keyset_limits_rust_working_memory_for_large_identity_sets", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
        return;
    }
    fixture(|call| {
        struct Dates(std::sync::atomic::AtomicUsize);
        impl crate::store::QueryRuntime for Dates {
            fn parse_date(&self, value: &JsString) -> f64 {
                self.0.fetch_add(1, Ordering::Relaxed);
                crate::native_runtime::parse_date(value)
            }
            fn locale_lowercase(&self, value: &JsString) -> IvyResult<JsString> {
                <NativeRuntime as crate::store::QueryRuntime>::locale_lowercase(
                    &NativeRuntime,
                    value,
                )
            }
        }
        let dates = Arc::new(Dates(std::sync::atomic::AtomicUsize::new(0)));
        call.store.bind_query_runtime(dates.clone());
        for index in 0..8192 {
            insert(
                call,
                "host",
                &format!("id{index:05}"),
                "fixture",
                "1970-01-01T00:00:00.000Z",
                0,
            );
        }
        let params = json(serde_json::json!({"limit":2}));
        let before = LIVE.load(Ordering::Relaxed);
        PEAK.store(before, Ordering::Relaxed);
        let first = list(call, &params).unwrap();
        let first_peak = PEAK.load(Ordering::Relaxed) - before;
        let first_dates = dates.0.swap(0, Ordering::Relaxed);
        let mut query = params.clone();
        query.insert("cursor", first.get("nextCursor").unwrap().clone());
        let before = LIVE.load(Ordering::Relaxed);
        PEAK.store(before, Ordering::Relaxed);
        let second = list(call, &query).unwrap();
        let second_peak = PEAK.load(Ordering::Relaxed) - before;
        let second_dates = dates.0.load(Ordering::Relaxed);
        for (page, ids) in [
            (&first, ["id00000", "id00001"]),
            (&second, ["id00002", "id00003"]),
        ] {
            assert_eq!(items(page).len(), 2);
            for (record, id) in items(page).iter().zip(ids) {
                assert_eq!(
                    record.get("record").unwrap().get("deploymentId"),
                    Some(&id.into())
                );
            }
            assert!(!page.get("nextCursor").unwrap().is_null());
        }
        eprintln!(
            "Hive deployment SQL keyset Rust allocation peak: first page {first_peak} bytes, second page {second_peak} bytes for 8192 identities"
        );
        // Includes Rust-side projection/cache/page work; SQLite's native heap is separate.
        assert!(
            first_peak < 256 * 1024 && second_peak < 256 * 1024,
            "The page materialized the full identity set: {first_peak}, {second_peak}"
        );
        assert!(
            first_dates <= 8192 && second_dates <= 8192,
            "Cursor filtering re-evaluated projected dates: {first_dates}, {second_dates}"
        );
    });
}
