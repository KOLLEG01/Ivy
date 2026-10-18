use ivy_agent_manager::{skills_output::inspect_ownership, skills_prepare::prepare_skills};
use ivy_contracts::{
    IvyError, IvyJson, JsString, Outcome,
    hash::digest,
    json::{canonical, parse},
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    collections::VecDeque,
    path::PathBuf,
    process::Command,
    sync::{Mutex, OnceLock},
};

fn oracle() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        let output = Command::new("node")
            .arg("tests/fixtures/agent-skills-output-oracle.mjs")
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        parse(
            std::str::from_utf8(&output.stdout).unwrap(),
            4 * 1024 * 1024,
        )
        .unwrap()
    })
}
struct Client(Mutex<VecDeque<IvyJson>>);
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let row = self
                .0
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request");
            assert_eq!(row.get("method"), Some(&IvyJson::from(method)));
            assert_eq!(row.get("params"), Some(&params));
            if let Some(error) = row.get("error") {
                return Err(IvyError::new(
                    error.get("code").unwrap().as_str().unwrap(),
                    "fixture failure",
                )
                .with_outcome(
                    if error.get("outcome") == Some(&IvyJson::from("unknown")) {
                        Outcome::Unknown
                    } else {
                        Outcome::NotExecuted
                    },
                ));
            }
            Ok(row.get("result").unwrap().clone())
        })
    }
}

#[tokio::test]
async fn authoritative_skills_preparation_matches_original_reads_and_version() {
    let rows = oracle().get("preparation").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 47);
    for (index, row) in rows.iter().enumerate() {
        let client = Client(Mutex::new(
            row.get("replies")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .cloned()
                .collect(),
        ));
        let actual = match prepare_skills(
            &client,
            row.get("defaults").unwrap(),
            row.get("hostId").unwrap().as_js_string().unwrap(),
            row.get("hive").unwrap().as_js_string().unwrap(),
        )
        .await
        {
            Ok(prepared) => IvyJson::object([(
                "result",
                IvyJson::object([
                    (
                        "groups",
                        IvyJson::Array(
                            prepared
                                .groups
                                .iter()
                                .map(|group| {
                                    IvyJson::object([
                                        ("name", IvyJson::String(group.name.clone())),
                                        (
                                            "files",
                                            IvyJson::Array(
                                                group
                                                    .files
                                                    .iter()
                                                    .map(|(name, bytes)| {
                                                        IvyJson::Array(vec![
                                                            IvyJson::String(name.clone()),
                                                            digest(bytes).into(),
                                                        ])
                                                    })
                                                    .collect(),
                                            ),
                                        ),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                    ("desiredVersion", prepared.desired_version.into()),
                    ("authority", prepared.authority.into()),
                ]),
            )]),
            Err(error) => IvyJson::object([("error", error.code.into())]),
        };
        assert_eq!(Some(&actual), row.get("expected"), "preparation {index}");
        assert!(
            client.0.lock().unwrap().is_empty(),
            "missing request {index}"
        );
    }
}

#[test]
fn skills_ownership_migration_collisions_and_retention_match_original_effects() {
    let rows = oracle().get("ownership").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 128);
    for (index, row) in rows.iter().enumerate() {
        let after: Vec<(JsString, String)> = row
            .get("after")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                let pair = entry.as_array().unwrap();
                (
                    pair[0].as_js_string().unwrap().clone(),
                    pair[1].as_str().unwrap(),
                )
            })
            .collect();
        let before = row.get("beforeTrees").unwrap();
        let tree = |name: &JsString| {
            let value = before.get_key(name).expect("unexpected tree inspection");
            if let Some(error) = value.get("error") {
                Err(IvyError::new(
                    error.as_str().unwrap(),
                    "fixture tree failure",
                ))
            } else if value.is_null() {
                Ok(None)
            } else {
                Ok(Some(value.as_str().unwrap()))
            }
        };
        let planned = inspect_ownership(
            row.get("state"),
            &row.get("authority").unwrap().as_str().unwrap(),
            &after.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
            tree,
        )
        .and_then(|ownership| {
            ownership.plan(
                &after,
                &row.get("desiredVersion").unwrap().as_str().unwrap(),
                tree,
            )
        });
        let actual = match planned {
            Err(error) => IvyJson::object([("error", error.code.into())]),
            Ok(planned) => {
                // Project the requested replacements onto the observed original
                // filesystem inventory; the oracle records actual Node effects.
                let mut trees = before.clone();
                for replacement in planned.replacements {
                    if !replacement.unchanged {
                        if replacement.owned {
                            trees.insert(replacement.source_name, IvyJson::Null);
                        }
                        let hash = after
                            .iter()
                            .find(|(name, _)| *name == replacement.name)
                            .unwrap()
                            .1
                            .clone();
                        trees.insert(replacement.name, hash.into());
                    }
                }
                IvyJson::object([(
                    "result",
                    // State files use atomic_json's canonical object order.
                    IvyJson::object([
                        (
                            "state",
                            parse(&canonical(&planned.state, usize::MAX).unwrap(), usize::MAX)
                                .unwrap(),
                        ),
                        ("trees", trees),
                    ]),
                )])
            }
        };
        assert_eq!(
            Some(&actual),
            row.get("expected"),
            "ownership {index}: {:?}/{:?}",
            row.get("mode"),
            row.get("selection")
        );
    }
}
