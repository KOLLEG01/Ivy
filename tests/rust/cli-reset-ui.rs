use ivy_cli::{
    publish_ui::{FileAsset, UiBundle},
    reset_ui::{ResetUiBundle, republish_reset_uis, reset_bootstrap_contracts, reset_ui_bundles},
};
use ivy_contracts::{
    IvyError, IvyJson as J, IvyResult, JsString, Outcome,
    json::{parse, stringify},
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{path::PathBuf, sync::Mutex};
#[path = "cli-reset-ui-auth.rs"]
mod auth;

fn json(text: &str) -> J {
    parse(text, usize::MAX).unwrap()
}
fn captured(value: IvyResult<J>) -> J {
    match value {
        Ok(value) => J::object([("ok", true.into()), ("value", value)]),
        Err(error) => J::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
struct Fake {
    script: J,
    inspections: Mutex<usize>,
    trace: Mutex<Vec<J>>,
}
impl RpcClient for Fake {
    fn request<'a>(&'a self, method: &'a str, params: J, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            self.trace
                .lock()
                .unwrap()
                .push(J::object([("method", method.into()), ("params", params)]));
            if self.script.get("failureMethod") == Some(&J::from(method)) {
                return Err(IvyError::new("outcome_unknown", "Synthetic lost reply.")
                    .with_outcome(Outcome::Unknown));
            }
            if method != "uis.inspect" {
                return Ok(J::object([
                    ("fixture", true.into()),
                    ("method", method.into()),
                ]));
            }
            let mut index = self.inspections.lock().unwrap();
            let reply = &self.script.get("inspections").unwrap().as_array().unwrap()[*index];
            *index += 1;
            if let Some(error) = reply.get("error") {
                return Err(IvyError::new_js(
                    error.get("code").unwrap().as_js_string().unwrap().clone(),
                    error
                        .get("message")
                        .unwrap()
                        .as_js_string()
                        .unwrap()
                        .clone(),
                )
                .with_outcome(
                    match error.get("outcome").unwrap().as_str()?.as_str() {
                        "unknown" => Outcome::Unknown,
                        "completed" => Outcome::Completed,
                        _ => Outcome::NotExecuted,
                    },
                ));
            }
            Ok(reply.get("result").unwrap().clone())
        })
    }
}
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let parent = std::fs::canonicalize(
            std::env::var_os("IVY_TEST_TEMP")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir),
        )
        .unwrap();
        let path = std::fs::canonicalize(&self.0).unwrap();
        assert_eq!(path.parent(), Some(parent.as_path()));
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-reset-ui-")
        );
        std::fs::remove_dir_all(path).unwrap();
    }
}
fn bundles(value: &str) -> Vec<ResetUiBundle> {
    json(value)
        .as_array()
        .unwrap()
        .iter()
        .map(|value| {
            let checked = value.get("checkedBundle").unwrap();
            ResetUiBundle {
                directory: PathBuf::from(value.get("directory").unwrap().as_str().unwrap()),
                definition: value.get("definition").unwrap().clone(),
                checked_bundle: UiBundle {
                    release_id: checked.get("releaseId").unwrap().as_str().unwrap(),
                    bundle_hash: checked.get("bundleHash").unwrap().as_str().unwrap(),
                    assets: checked
                        .get("assets")
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|asset| FileAsset {
                            path: asset.get("path").unwrap().as_str().unwrap(),
                            media_type: asset.get("mediaType").unwrap().as_str().unwrap(),
                            content_hash: asset.get("contentHash").unwrap().as_str().unwrap(),
                            byte_length: asset.get("byteLength").unwrap().as_f64().unwrap()
                                as usize,
                        })
                        .collect(),
                },
            }
        })
        .collect()
}

#[tokio::test]
async fn reset_discovery_and_publication_match_original_functions() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/host-reset-ui-oracle.mjs",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    if let Some(path) = std::env::var_os("IVY_RESET_UI_EVIDENCE") {
        std::fs::write(path, &output.stdout).unwrap();
    }
    let data: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let _fixture = Fixture(PathBuf::from(data["root"].as_str().unwrap()));
    let scans = data["scans"].as_array().unwrap();
    assert_eq!(scans.len(), 28);
    for row in scans {
        let ids: Vec<JsString> = json(row["uiIds"].as_str().unwrap())
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_js_string().unwrap().clone())
            .collect();
        let value = reset_ui_bundles(&PathBuf::from(row["distribution"].as_str().unwrap()), &ids)
            .await
            .map(|bundles| {
                J::Array(
                    bundles
                        .into_iter()
                        .map(|bundle| {
                            J::object([
                                ("directory", bundle.directory.to_str().unwrap().into()),
                                ("definition", bundle.definition),
                                ("checkedBundle", bundle.checked_bundle.to_json()),
                            ])
                        })
                        .collect(),
                )
            });
        assert_eq!(
            stringify(&captured(value), usize::MAX).unwrap(),
            row["expected"].as_str().unwrap(),
            "scan {}",
            row["name"]
        );
    }
    let publications = data["publications"].as_array().unwrap();
    assert_eq!(publications.len(), 19);
    for row in publications {
        let bundles = bundles(row["bundles"].as_str().unwrap());
        let fake = Fake {
            script: json(row["script"].as_str().unwrap()),
            inspections: Mutex::new(0),
            trace: Mutex::new(Vec::new()),
        };
        let mutation = json(row["mutationId"].as_str().unwrap());
        let result = republish_reset_uis(&fake, &bundles, mutation.as_js_string().unwrap())
            .await
            .map(|value| J::Array(value.into_iter().map(J::String).collect()));
        if matches!(
            row["name"].as_str(),
            Some("null-inspection" | "false-inspection")
        ) {
            assert_eq!(result.unwrap_err().code, "internal_error");
            assert_eq!(
                fake.trace.lock().unwrap().len(),
                1,
                "A malformed inspection must fail before publication."
            );
            continue;
        }
        assert_eq!(
            stringify(&captured(result), usize::MAX).unwrap(),
            row["expected"].as_str().unwrap(),
            "publication {}",
            row["name"]
        );
        assert_eq!(
            stringify(&J::Array(fake.trace.into_inner().unwrap()), usize::MAX).unwrap(),
            row["trace"].as_str().unwrap(),
            "trace {}",
            row["name"]
        );
    }
    let selections = data["selections"].as_array().unwrap();
    assert_eq!(selections.len(), 16);
    for row in selections {
        let bundles = bundles(row["bundles"].as_str().unwrap());
        let chat = json(row["chat"].as_str().unwrap());
        let board = json(row["board"].as_str().unwrap());
        let catalog = chat
            .as_array()
            .unwrap()
            .iter()
            .chain(board.as_array().unwrap())
            .cloned()
            .collect::<Vec<_>>();
        let result = reset_bootstrap_contracts(&bundles, &catalog).map(J::Array);
        let mut expected = json(row["expected"].as_str().unwrap());
        // Native contract order is UTF-16 key/version order; definition identity
        // and selection authority remain the original reference's exact values.
        if let Some(values) = expected.get("value").and_then(J::as_array) {
            let mut values = values.to_vec();
            values.sort_by_key(|value| {
                (
                    value.get("key").unwrap().as_str().unwrap(),
                    value.get("version").unwrap().as_str().unwrap(),
                )
            });
            expected.insert("value", J::Array(values));
        }
        assert_eq!(
            stringify(&captured(result), usize::MAX).unwrap(),
            stringify(&expected, usize::MAX).unwrap(),
            "selection {}",
            row["name"]
        );
    }
}
