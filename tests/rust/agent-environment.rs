use ivy_agent_manager::environment::{
    EnvironmentKind, read_environment_document, select_environment_document,
};
use ivy_contracts::{IvyJson as Json, IvyResult};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::sync::Mutex;

fn document(enabled: Json) -> Json {
    Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("hostId", Json::Null),
        ("enabled", enabled),
        ("servers", Json::Array(vec![])),
    ])
}
#[test]
fn disabled_host_override_and_inherited_versions_are_retained() {
    let defaults = document(true.into());
    let global = Json::object([
        ("document", defaults.clone()),
        ("revision", Json::Number(1.0)),
    ]);
    let disabled = document(false.into());
    let host = Json::object([
        ("document", disabled.clone()),
        ("revision", Json::Number(2.0)),
    ]);
    let selected = select_environment_document(&defaults, Some(&global), Some(&host)).unwrap();
    assert_eq!(selected.document, disabled);
    let inherited = Json::object([
        ("document", document(Json::Null)),
        ("revision", Json::Number(3.0)),
    ]);
    let fallback = select_environment_document(&defaults, Some(&global), Some(&inherited)).unwrap();
    assert_eq!(fallback.document, defaults);
    assert_ne!(selected.version, fallback.version);
}
struct Client {
    version: &'static str,
    calls: Mutex<Vec<String>>,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(method.into());
            if method == "objects.stat" {
                return Ok(Json::object([
                    ("id", "intent".into()),
                    ("currentRevision", Json::Number(7.0)),
                    ("effectivelyArchived", false.into()),
                ]));
            }
            assert_eq!(params.get("revision"), Some(&Json::Number(7.0)));
            Ok(Json::object([
                (
                    "object",
                    Json::object([("contractKey", "agent/mcp-configuration".into())]),
                ),
                (
                    "revision",
                    Json::object([
                        ("contractVersion", self.version.into()),
                        ("contentHash", "sha256:fixture".into()),
                    ]),
                ),
                (
                    "content",
                    Json::object([
                        ("encoding", "json".into()),
                        ("value", document(true.into())),
                    ]),
                ),
            ]))
        })
    }
}
#[tokio::test]
async fn mcp_reads_the_pinned_current_contract_and_refuses_another_version() -> IvyResult<()> {
    for (version, accepted) in [("1.1.0", true), ("1.0.0", false)] {
        let client = Client {
            version,
            calls: Mutex::new(vec![]),
        };
        let result = read_environment_document(&client, EnvironmentKind::Mcp, None).await;
        assert_eq!(result.is_ok(), accepted);
        if !accepted {
            assert_eq!(result.unwrap_err().code, "contract_mismatch");
        }
        assert_eq!(
            *client.calls.lock().unwrap(),
            ["objects.stat", "objects.read"]
        );
    }
    Ok(())
}
