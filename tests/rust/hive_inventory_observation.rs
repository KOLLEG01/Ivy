use super::*;
use ivy_hive::native_runtime::NativeRuntime;

struct Fixture {
    kernel: Kernel,
    clock: Rc<Cell<f64>>,
    path: std::path::PathBuf,
    _root: Root,
}
impl Fixture {
    fn new() -> Self {
        let root = Root::new();
        let path = root.0.join("observations.sqlite");
        let mut store = Store::open(&path).unwrap();
        store
            .configure_credentials(&[ivy_hive::store::Credential {
                principal_id: "alpha".into(),
                digest: format!("sha256:{}", "a".repeat(64)).into(),
            }])
            .unwrap();
        store.bind_query_runtime(Arc::new(NativeRuntime));
        let clock = Rc::new(Cell::new(100_000.0));
        let runtime = Rc::new(NativeRuntime);
        let registry = Rc::new(RefCell::new(
            Registry::new(&mut store, runtime.as_ref(), clock.get()).unwrap(),
        ));
        let discovery = Discovery::new(runtime.as_ref()).unwrap();
        let configurations = Configurations::new(&mut store).unwrap();
        let mut configuration = options();
        configuration.validate_messages = true;
        let owner_clock = clock.clone();
        let kernel = Kernel::compose(
            Parts {
                store,
                registry,
                discovery,
                configurations,
                ui_files: UiFiles::new(root.0.join("files")),
            },
            configuration,
            runtime,
            Rc::new(move || owner_clock.get()),
        )
        .unwrap();
        let mut fixture = Self {
            kernel,
            clock,
            path,
            _root: root,
        };
        fixture.call(connect("node", Some(1.0))).unwrap();
        let mut registry = catalog();
        registry["requiredContracts"] = json!([]);
        fixture
            .call(service_request("registry.sync", "node", registry))
            .unwrap();
        fixture
            .call(service_request(
                "service.heartbeat",
                "node",
                json!({"ready":true,"diagnostics":[]}),
            ))
            .unwrap();
        fixture
    }
    fn call(&mut self, step: Value) -> IvyResult<IvyJson> {
        self.kernel
            .execute(
                &IvyJson::from_serde(&step["context"]).unwrap(),
                &IvyJson::from_serde(&step["request"]).unwrap(),
            )
            .map(|reply| reply.required("value").unwrap().clone())
    }
    fn sync(&mut self, parameters: Value) -> IvyResult<IvyJson> {
        // Exercise the storage handler independently of the wire schema.
        ivy_hive::inventory::sync(
            &mut self.kernel.parts.store,
            &mut self.kernel.parts.registry.borrow_mut(),
            &"node".into(),
            &IvyJson::from_serde(&parameters).unwrap(),
            &NativeRuntime,
            self.clock.get(),
        )
    }
    fn state(&self) -> (f64, String, String, i64) {
        self.kernel.parts.store.connection().query_row(
            "SELECT s.revision,s.observed_at,json_extract(n.status_json,'$.lastObservationAt'),(SELECT count(*) FROM inventory) FROM inventory_sets s JOIN service_nodes n ON n.id=s.node_id WHERE s.node_id='node' AND s.kind='item'",
            [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).unwrap()
    }
}
fn batch(revision: u64, observed: &str, entries: Vec<Value>) -> Value {
    json!({"namespace":"fixture","kind":"item","schemaVersion":"1.0.0","mode":"snapshot","snapshotRevision":revision,"observedAt":observed,"entries":entries})
}
const SOURCE: &str = "1970-01-01T00:01:40.000Z";

#[test]
fn source_batch_time_survives_empty_snapshots_removal_only_deltas_replay_and_reopen() {
    let mut f = Fixture::new();
    f.clock.set(200_000.0);
    let empty = batch(1, SOURCE, vec![]);
    let first = f.sync(empty.clone()).unwrap();
    assert_eq!(
        first.get("observedAt"),
        Some(&SOURCE.into()),
        "An empty source observation acquired its delivery time"
    );
    assert_eq!(f.state(), (1.0, SOURCE.into(), SOURCE.into(), 0));
    f.clock.set(300_000.0);
    assert_eq!(f.sync(empty.clone()).unwrap(), first);
    let mut changed_time = empty;
    changed_time["observedAt"] = json!("1970-01-01T00:01:41.000Z");
    assert_eq!(f.sync(changed_time).unwrap_err().code, "revision_conflict");
    assert_eq!(f.state(), (1.0, SOURCE.into(), SOURCE.into(), 0));

    // Individual entries retain their own age; a batch timestamp cannot refresh them.
    let old = "1970-01-01T00:01:30.000Z";
    let mut original = entry("one", "Original", 1);
    original["observedAt"] = json!(old);
    f.call(service_request(
        "inventory.sync",
        "node",
        batch(2, SOURCE, vec![original]),
    ))
    .unwrap();
    f.call(service_request(
        "service.heartbeat",
        "node",
        json!({"ready":true,"diagnostics":[]}),
    ))
    .unwrap();
    let item = f.call(request("inventory.get", json!({"resourceRef":{"serviceNodeId":"node","namespace":"fixture","kind":"item","nativeId":"one"}}))).unwrap();
    assert_eq!(item.get("observedAt"), Some(&old.into()));
    assert_eq!(item.get("stale"), Some(&true.into()));

    let deletion_at = "1970-01-01T00:01:50.000Z";
    let mut deletion = batch(3, deletion_at, vec![]);
    deletion["mode"] = json!("delta");
    deletion["expectedRevision"] = json!(2);
    deletion["removedNativeIds"] = json!(["one"]);
    f.clock.set(400_000.0);
    let result = f
        .call(service_request("inventory.sync", "node", deletion.clone()))
        .unwrap();
    assert_eq!(result.get("observedAt"), Some(&deletion_at.into()));
    assert_eq!(f.state(), (3.0, deletion_at.into(), deletion_at.into(), 0));
    assert_eq!(f.sync(deletion).unwrap_err().code, "revision_conflict");
    let state = f.state();
    drop(f.kernel);
    let reopened = Store::open(&f.path).unwrap();
    let retained: (f64, String, String, i64) = reopened.connection().query_row(
        "SELECT s.revision,s.observed_at,json_extract(n.status_json,'$.lastObservationAt'),(SELECT count(*) FROM inventory) FROM inventory_sets s JOIN service_nodes n ON n.id=s.node_id WHERE s.node_id='node' AND s.kind='item'",
        [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
    ).unwrap();
    assert_eq!(retained, state);
}

#[test]
fn supplied_batch_time_is_calendar_checked_and_atomic_with_the_original_revision() {
    let mut f = Fixture::new();
    let initial = batch(1, SOURCE, vec![entry("one", "Original", 1)]);
    f.call(service_request("inventory.sync", "node", initial))
        .unwrap();
    let before = f.state();
    for value in [
        Value::Null,
        json!(4),
        json!("2024-02-30T00:00:00Z"),
        json!("1970-01-01T00:01:40+00:00"),
        json!("not a date"),
    ] {
        let mut invalid = batch(1, SOURCE, vec![]);
        invalid["observedAt"] = value;
        assert_eq!(
            f.call(service_request("inventory.sync", "node", invalid))
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
        assert_eq!(f.state(), before);
    }
    f.kernel.parts.store.connection().execute_batch("CREATE TRIGGER observation_failure BEFORE INSERT ON inventory WHEN NEW.native_id='two' BEGIN SELECT RAISE(ABORT,'fixture failure'); END;").unwrap();
    let next_at = "1970-01-01T00:01:50.000Z";
    let next = batch(2, next_at, vec![entry("two", "Replacement", 2)]);
    assert_eq!(
        f.call(service_request("inventory.sync", "node", next.clone()))
            .unwrap_err()
            .code,
        "internal_error"
    );
    assert_eq!(f.state(), before);
    f.kernel
        .parts
        .store
        .connection()
        .execute_batch("DROP TRIGGER observation_failure;")
        .unwrap();
    f.call(service_request("inventory.sync", "node", next))
        .unwrap();
    assert_eq!(f.state(), (2.0, next_at.into(), next_at.into(), 1));
}

#[test]
fn installed_publishers_without_batch_time_pass_real_admission_and_preserve_replay() {
    let mut f = Fixture::new();
    f.clock.set(200_000.0);
    let mut legacy = batch(1, SOURCE, vec![entry("one", "Original", 1)]);
    legacy.as_object_mut().unwrap().remove("observedAt");
    let first = f
        .call(service_request("inventory.sync", "node", legacy.clone()))
        .unwrap();
    assert_eq!(
        first.get("observedAt"),
        Some(&"1970-01-01T00:01:40Z".into())
    );
    f.clock.set(300_000.0);
    assert_eq!(
        f.call(service_request("inventory.sync", "node", legacy))
            .unwrap(),
        first
    );

    let mut removal = batch(2, SOURCE, vec![]);
    removal.as_object_mut().unwrap().remove("observedAt");
    removal["mode"] = json!("delta");
    removal["expectedRevision"] = json!(1);
    removal["removedNativeIds"] = json!(["one"]);
    f.call(service_request("inventory.sync", "node", removal.clone()))
        .unwrap();
    assert_eq!(
        f.state(),
        (
            2.0,
            "1970-01-01T00:05:00.000Z".into(),
            "1970-01-01T00:05:00.000Z".into(),
            0
        )
    );
    assert_eq!(
        f.call(service_request("inventory.sync", "node", removal))
            .unwrap_err()
            .code,
        "revision_conflict"
    );

    let mut empty = batch(3, SOURCE, vec![]);
    empty.as_object_mut().unwrap().remove("observedAt");
    let result = f
        .call(service_request("inventory.sync", "node", empty.clone()))
        .unwrap();
    f.clock.set(400_000.0);
    assert_eq!(
        f.call(service_request("inventory.sync", "node", empty))
            .unwrap(),
        result
    );
    f.call(service_request(
        "inventory.sync",
        "node",
        batch(4, SOURCE, vec![]),
    ))
    .unwrap();
    assert_eq!(f.state(), (4.0, SOURCE.into(), SOURCE.into(), 0));
}
