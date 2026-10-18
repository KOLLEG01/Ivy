use ivy_contracts::{IvyError, IvyJson, builtin_schema::json_equal, json::parse};
use ivy_data_collector::{
    model,
    runtime::{Context, Read, Runtime},
    schema::parse_schema,
    store::{CollectorStore, Limits},
};
use std::{cell::Cell, process::Command};

#[test]
fn runtime_mutations_and_reads_match_original_engine_receipts_and_effects() {
    let directory =
        std::env::temp_dir().join(format!("ivy-collector-runtime-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(directory.clone());
    let output = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/data-collector-runtime-oracle.mjs",
            "runtime",
        ])
        .arg(directory.join("node.sqlite"))
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let store = CollectorStore::open(
        &directory.join("rust.sqlite"),
        Limits::default(),
        "1970-01-01T00:16:40.000Z",
    )
    .unwrap();
    let settings = parse_schema(
        "settingsSchema",
        &parse(r#"{"maximumTasks":1}"#, usize::MAX).unwrap(),
    )
    .unwrap();
    let runtime = Runtime {
        store: &store,
        settings: &settings,
        root_id: "root",
    };
    let active = Cell::new(false);
    let clears = Cell::new(0);
    let cancelled = Cell::new(0);
    let clear_failure = Cell::new(false);
    for (index, row) in rows.as_array().unwrap().iter().enumerate() {
        let step = row.get("step").unwrap();
        let input = step.get("input").unwrap();
        let kind = model::text(step, "kind").unwrap();
        let actual = (|| {
            Ok(match kind.as_str() {
                "read" => match runtime.read(input)? {
                    Read::Local(value) => value,
                    Read::Remote { method, params } => {
                        IvyJson::object([("method", method.into()), ("params", params)])
                    }
                },
                "update" => {
                    let context = step.get("context").map(|c| Context {
                        caller: c.get("callerPrincipalId").unwrap().as_js_string().unwrap(),
                        operation_id: c.get("operationId").and_then(IvyJson::as_js_string),
                    });
                    let result = runtime.update(
                        input,
                        context,
                        1000000,
                        "1970-01-01T00:16:40.000Z",
                        |_| active.get(),
                        |_| {
                            clears.set(clears.get() + 1);
                            if clear_failure.get() {
                                Err(IvyError::new("fixture_cleanup", "Cleanup failed."))
                            } else {
                                Ok(())
                            }
                        },
                    )?;
                    if result.disabled.is_some() && active.get() {
                        cancelled.set(cancelled.get() + 1);
                    }
                    result.value
                }
                "patchRun" => {
                    let id = model::sqlite_key(input.get("id").unwrap())?;
                    let mut run = store.run(&id)?.unwrap();
                    for (key, value) in input.as_object().unwrap() {
                        run.insert(key.clone(), value.clone());
                    }
                    store.save_run(&run)?;
                    IvyJson::Null
                }
                "active" => {
                    active.set(input.as_bool().unwrap());
                    IvyJson::Null
                }
                "bindResult" => {
                    let mut row = store.get("task", false)?.unwrap();
                    row.insert("resultObjectId", input.clone());
                    store.save(&row)?;
                    IvyJson::Null
                }
                "clearFailure" => {
                    clear_failure.set(input.as_bool().unwrap());
                    IvyJson::Null
                }
                _ => panic!("unknown fixture command"),
            })
        })();
        match actual {
            Ok(value) => {
                assert!(
                    row.get("error").unwrap().is_null(),
                    "case {index} expected error"
                );
                assert!(
                    json_equal(&value, row.get("value").unwrap()),
                    "case {index} returned {value:?}"
                );
            }
            Err(error) => {
                let expected = row.get("error").unwrap();
                assert_eq!(
                    Some(&IvyJson::from(error.code)),
                    expected.get("code"),
                    "case {index}"
                );
                assert_eq!(
                    Some(&IvyJson::from(error.message)),
                    expected.get("message"),
                    "case {index}"
                );
                assert_eq!(
                    Some(&IvyJson::from(error.outcome.as_str())),
                    expected.get("outcome"),
                    "case {index}"
                );
            }
        }
        assert_eq!(
            row.get("clears").unwrap().as_f64().unwrap(),
            clears.get() as f64,
            "case {index}"
        );
        assert_eq!(
            row.get("cancelled").unwrap().as_f64().unwrap(),
            cancelled.get() as f64,
            "case {index}"
        );
        assert!(
            json_equal(
                &IvyJson::Array(store.list(true).unwrap()),
                row.get("tasks").unwrap()
            ),
            "case {index} tasks"
        );
        let runs = ["queued", "running", "publishing", "failed", "cancelled"]
            .into_iter()
            .flat_map(|status| store.runs(status).unwrap())
            .collect();
        assert!(
            json_equal(&IvyJson::Array(runs), row.get("runs").unwrap()),
            "case {index} runs"
        );
    }
}
