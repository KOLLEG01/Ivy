use ivy_cli::publish_ui::{FileAsset, PublishUiInput, UiBundle, publish_ui, read_ui_bundle};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, Outcome,
    json::{parse, stringify},
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{path::PathBuf, sync::Mutex};

fn json(text: &str) -> IvyJson {
    parse(text, usize::MAX).unwrap()
}
fn captured(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
struct Fake {
    mode: String,
    trace: Mutex<Vec<IvyJson>>,
}
impl RpcClient for Fake {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.trace.lock().unwrap().push(IvyJson::object([
                ("method", method.into()),
                ("params", params),
            ]));
            if matches!(
                (self.mode.as_str(), method),
                ("fail-register", "contracts.register")
                    | ("fail-asset", "uis.stageAsset")
                    | ("fail-deploy", "uis.deploy")
            ) {
                return Err(IvyError::new("outcome_unknown", "Synthetic lost reply.")
                    .with_outcome(Outcome::Unknown));
            }
            Ok(IvyJson::object([
                ("fixture", true.into()),
                ("method", method.into()),
            ]))
        })
    }
}
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let parent = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let resolved = std::fs::canonicalize(&self.0).unwrap();
        assert!(
            resolved.parent() == Some(parent.as_path())
                && self
                    .0
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("ivy-rust-publication-")
        );
        std::fs::remove_dir_all(&resolved).unwrap();
    }
}

#[tokio::test]
async fn filesystem_publication_and_command_match_typescript() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/ui-publish-oracle.mjs",
        ])
        .env(
            "IVY_PUBLISH_TEST_BINARY",
            env!("CARGO_BIN_EXE_ivy-publish-ui"),
        )
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let _fixture = Fixture(PathBuf::from(data["root"].as_str().unwrap()));
    let rows = data["rows"].as_array().unwrap();
    assert!(rows.len() > 35);
    for row in rows {
        let directory = PathBuf::from(row["directory"].as_str().unwrap());
        let definition = json(row["definition"].as_str().unwrap());
        let result = if row["kind"] == "scan" {
            read_ui_bundle(&directory, &definition).map(|b| b.to_json())
        } else {
            let saved = &row["checkedBundle"];
            let bundle = UiBundle {
                assets: saved["assets"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| FileAsset {
                        path: v["path"].as_str().unwrap().into(),
                        media_type: v["mediaType"].as_str().unwrap().into(),
                        content_hash: v["contentHash"].as_str().unwrap().into(),
                        byte_length: v["byteLength"].as_u64().unwrap() as usize,
                    })
                    .collect(),
                bundle_hash: saved["bundleHash"].as_str().unwrap().into(),
                release_id: saved["releaseId"].as_str().unwrap().into(),
            };
            let mutation = json(row["mutationId"].as_str().unwrap());
            let expected = json(row["expectedReleaseId"].as_str().unwrap());
            let client = Fake {
                mode: row["name"].as_str().unwrap().into(),
                trace: Mutex::new(Vec::new()),
            };
            let result = publish_ui(
                &client,
                PublishUiInput {
                    directory: &directory,
                    definition: &definition,
                    mutation_id: mutation.as_js_string().unwrap(),
                    expected_release_id: expected.as_js_string(),
                    checked_bundle: Some(&bundle),
                },
            )
            .await;
            assert_eq!(
                stringify(
                    &IvyJson::Array(client.trace.into_inner().unwrap()),
                    usize::MAX
                )
                .unwrap(),
                row["trace"].as_str().unwrap(),
                "trace {}",
                row["name"]
            );
            result
        };
        assert_eq!(
            stringify(&captured(result), usize::MAX).unwrap(),
            row["expected"].as_str().unwrap(),
            "{}",
            row["name"]
        );
    }
    let commands = data["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 15);
    for row in commands {
        assert_eq!(row["actual"], row["expected"], "command {}", row["name"]);
        assert_eq!(
            row["actualTrace"], row["expectedTrace"],
            "command trace {}",
            row["name"]
        );
    }
    assert_eq!(data["unicode"], "17.0");
    let mappings: std::collections::HashMap<_, _> = data["nfc"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (v[0].as_u64().unwrap() as u32, v[1].as_str().unwrap()))
        .collect();
    let normalizer = icu_normalizer::ComposingNormalizer::new_nfc();
    for cp in 0..=0x10ffff {
        if let Some(c) = char::from_u32(cp) {
            let text = c.to_string();
            assert_eq!(
                normalizer.normalize(&text).as_ref(),
                mappings.get(&cp).copied().unwrap_or(&text),
                "Unicode NFC U+{cp:04X}"
            );
        }
    }
}
