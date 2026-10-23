use ivy_contracts::{IvyJson as J, JsString};
use ivy_host_runtime::{layout, node_path, service_storage};
use ivy_store::rusqlite::Connection;
use std::{fs, path::PathBuf};
#[path = "host-service-fixture.rs"]
#[allow(dead_code)]
mod fixture;
use fixture::{ServiceFixture, host_configuration};
fn contract(min: f64, max: f64, write: f64) -> J {
    J::object([
        ("minReadableFormat", J::Number(min)),
        ("maxReadableFormat", J::Number(max)),
        ("writeFormat", J::Number(write)),
    ])
}
fn plan(component: &str, storage: J) -> J {
    J::object([("componentId", component.into()), ("storage", storage)])
}
fn database(config: &J) -> PathBuf {
    node_path::to_native(
        layout::service_paths(config, &"fixture".into())
            .unwrap()
            .required("data")
            .unwrap()
            .as_js_string()
            .unwrap(),
    )
    .join("phone-commands.sqlite")
}
async fn verify(config: &J, storage: J, witness: Option<f64>) -> ivy_contracts::IvyResult<()> {
    service_storage::verify_service_storage(
        config.clone(),
        "fixture".into(),
        plan("phone-bridge", storage),
        witness,
    )
    .await
}

#[tokio::test]
async fn storage_contract_is_typed_before_missing_database_and_unrelated_names_are_ordinary() {
    let f = ServiceFixture::new();
    let config = host_configuration(&f);
    for storage in [
        J::Null,
        false.into(),
        "2".into(),
        J::object([
            ("minReadableFormat", "1".into()),
            ("maxReadableFormat", J::Number(3.0)),
            ("writeFormat", J::Number(2.0)),
        ]),
        contract(3.0, 1.0, 2.0),
    ] {
        assert_eq!(
            verify(&config, storage, None).await.unwrap_err().code,
            "incompatible_storage"
        );
    }
    verify(&config, contract(1.0, 3.0, 2.0), None)
        .await
        .unwrap();
    for component in ["other", "constructor", "toString", "__proto__"] {
        service_storage::verify_service_storage(
            J::Null,
            JsString::from("unused"),
            plan(component, J::Null),
            None,
        )
        .await
        .unwrap();
    }
    assert!(!database(&config).exists());
}
#[tokio::test]
async fn storage_checks_exact_write_format_without_changing_database_bytes() {
    for version in [0, 1, 2, 3, -1] {
        let f = ServiceFixture::new();
        let config = host_configuration(&f);
        let file = database(&config);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let db = Connection::open(&file).unwrap();
        db.execute_batch(&format!("PRAGMA user_version={version}"))
            .unwrap();
        drop(db);
        let before = fs::read(&file).unwrap();
        let result = verify(&config, contract(1.0, 3.0, 2.0), None).await;
        assert_eq!(result.is_ok(), matches!(version, 0 | 2), "{version}");
        assert_eq!(fs::read(&file).unwrap(), before);
    }
}
#[tokio::test]
async fn busy_reads_allow_only_the_supplied_generation_witness_and_corruption_never_uses_it() {
    let f = ServiceFixture::new();
    let config = host_configuration(&f);
    let file = database(&config);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let db = Connection::open(&file).unwrap();
    db.execute_batch("PRAGMA user_version=2; PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE;")
        .unwrap();
    assert!(
        verify(&config, contract(2.0, 2.0, 2.0), None)
            .await
            .is_err()
    );
    verify(&config, contract(2.0, 2.0, 2.0), Some(2.0))
        .await
        .unwrap();
    assert!(
        verify(&config, contract(2.0, 2.0, 2.0), Some(3.0))
            .await
            .is_err()
    );
    db.execute_batch("ROLLBACK;").unwrap();
    drop(db);
    fs::write(&file, b"not sqlite").unwrap();
    assert!(
        verify(&config, contract(2.0, 2.0, 2.0), Some(2.0))
            .await
            .is_err()
    );
    assert_eq!(fs::read(&file).unwrap(), b"not sqlite");
}
