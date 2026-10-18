use ivy_agent_manager::mcp_prepare::prepare_mcp;
use ivy_contracts::{
    IvyError, IvyJson, Outcome,
    json::{canonical, parse},
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{collections::VecDeque, path::PathBuf, process::Command, sync::Mutex};
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
                    error
                        .get("code")
                        .unwrap()
                        .as_js_string()
                        .unwrap()
                        .to_utf8()
                        .unwrap(),
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
async fn preparation_order_failures_and_output_identity_match_original_manager() {
    let output = Command::new("node")
        .arg("tests/fixtures/agent-mcp-prepare-oracle.mjs")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        2 * 1024 * 1024,
    )
    .unwrap();
    let rows = data.as_array().unwrap();
    assert_eq!(rows.len(), 52);
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
        let js = |key| row.get(key).unwrap().as_js_string().unwrap();
        let actual = match prepare_mcp(
            &client,
            row.get("defaults").unwrap(),
            js("hostId"),
            js("hive"),
            js("credential"),
        )
        .await
        {
            Ok(prepared) => {
                let planned = prepared.plan("", None).unwrap();
                IvyJson::object([(
                    "result",
                    IvyJson::object([
                        ("text", String::from_utf8(planned.bytes).unwrap().into()),
                        // The original fixture observes the canonical atomic_json file.
                        (
                            "state",
                            parse(&canonical(&planned.state, usize::MAX).unwrap(), usize::MAX)
                                .unwrap(),
                        ),
                        ("applied", planned.applied.into()),
                        (
                            "managedBytes",
                            IvyJson::Number(planned.managed_bytes as f64),
                        ),
                        ("desiredVersion", prepared.desired_version.into()),
                    ]),
                )])
            }
            Err(error) => {
                let state = if [
                    "target_conflict",
                    "limit_exceeded",
                    "contract_mismatch",
                    "access_denied",
                    "invalid_arguments",
                ]
                .contains(&error.code.as_str())
                {
                    "conflict"
                } else {
                    "pending"
                };
                IvyJson::object([(
                    "error",
                    IvyJson::object([("code", error.code.into()), ("state", state.into())]),
                )])
            }
        };
        assert_eq!(
            &actual,
            row.get("expected").unwrap(),
            "Case {index}: {:?}",
            row.get("mode")
        );
        assert!(
            client.0.lock().unwrap().is_empty(),
            "Missing request in {index}"
        );
    }
}
