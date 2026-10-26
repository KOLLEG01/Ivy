use ivy_agent_manager::{
    claude_mcp::{ClaudeMcpManager, ClaudeMcpSettings},
    mcp_credentials::{McpCredential, McpCredentials, resolve_mcp_server},
    mcp_manager::{McpManager, McpSettings},
    mcp_render::hive_mcp_urls,
};
use ivy_contracts::{IvyError, IvyJson as Json, agent_schema::validate_agent, json::stringify};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{fs, path::PathBuf, sync::Arc};

struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
struct Client;
impl RpcClient for Client {
    fn request<'a>(&'a self, method: &'a str, _: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            match method {
                "objects.stat" => Err(IvyError::new("not_found", "No override")),
                "openai.getConfiguration" => Ok(Json::object([
                    ("revision", Json::Number(0.0)),
                    (
                        "configuration",
                        Json::object([("schemaVersion", Json::Number(1.0)), ("appId", Json::Null)]),
                    ),
                ])),
                _ => panic!("Unexpected request: {method}"),
            }
        })
    }
}
fn credentials(token: &str) -> McpCredentials {
    [(
        "external".into(),
        McpCredential {
            url: "https://tools.example/mcp?tenant=one".into(),
            bearer_token: token.into(),
        },
    )]
    .into()
}
fn server(url: &str, mode: &str) -> Json {
    let mut value = Json::object([
        ("name", "external".into()),
        ("url", url.into()),
        ("enabled", true.into()),
        ("authentication", mode.into()),
        ("startupTimeoutSeconds", Json::Number(30.0)),
        ("toolTimeoutSeconds", Json::Number(60.0)),
    ]);
    if mode == "bearer" {
        value.insert("credentialId", "external".into());
    }
    value
}
fn document(server: Json) -> Json {
    Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("hostId", Json::Null),
        ("enabled", true.into()),
        ("servers", Json::Array(vec![server])),
    ])
}

#[test]
fn credential_scope_and_token_validation_refuse_disclosure() {
    let urls = hive_mcp_urls(&"https://hive.example".into()).unwrap();
    let hive = "hive-synthetic".into();
    let good = server("https://tools.example/mcp?tenant=one", "bearer");
    let resolve = |value: &Json, credentials: &McpCredentials| {
        resolve_mcp_server(value, &urls, &hive, credentials).map(|_| ())
    };
    validate_agent("McpDocument", &document(good.clone())).unwrap();
    assert!(resolve(&good, &credentials("synthetic-1._~+/==")).is_ok());
    assert_eq!(
        resolve(&good, &Default::default()).unwrap_err().code,
        "access_denied"
    );
    for url in [
        "https://tools.example/mcp?tenant=two",
        "https://tools.example/other",
        "https://other.example/mcp",
    ] {
        assert_eq!(
            resolve(&server(url, "bearer"), &credentials("synthetic"))
                .unwrap_err()
                .code,
            "access_denied"
        );
    }
    for token in ["", "bad\r\nHeader:value", "a=b", " with-space", "\u{e9}"] {
        assert_eq!(
            resolve(&good, &credentials(token)).unwrap_err().code,
            "invalid_arguments"
        );
    }
    assert!(resolve(&good, &credentials(&"a".repeat(16385))).is_err());
    assert_eq!(
        resolve(
            &server("https://tools.example/mcp", "agent-manager"),
            &Default::default()
        )
        .unwrap_err()
        .code,
        "access_denied"
    );
    assert!(
        resolve(
            &server("https://hive.example/mcp", "agent-manager"),
            &Default::default()
        )
        .is_ok()
    );
    assert!(
        resolve(
            &server("http://tools.example/mcp", "none"),
            &Default::default()
        )
        .is_err()
    );
    let mut unrelated = server("https://hive.example/mcp", "agent-manager");
    unrelated.insert("credentialId", "external".into());
    assert!(resolve(&unrelated, &credentials("synthetic")).is_err());
}

#[tokio::test]
async fn both_backends_project_and_rotate_the_selected_token_without_public_secrets() {
    let root = Root(std::env::temp_dir().join(format!("ivy-mcp-{}", uuid::Uuid::new_v4())));
    fs::create_dir(&root.0).unwrap();
    let selected = document(server("https://tools.example/mcp?tenant=one", "bearer"));
    for claude in [false, true] {
        let home = root.0.join(if claude { "claude" } else { "codex" });
        fs::create_dir(&home).unwrap();
        let target = home.join(if claude { "mcp.json" } else { "config.toml" });
        let mut previous = None;
        for token in ["synthetic-first", "synthetic-rotated"] {
            let status = if claude {
                let manager = ClaudeMcpManager::new(ClaudeMcpSettings {
                    target: ivy_host_runtime::node_path::from_native(&target),
                    host_id: "fixture".into(),
                    hive_url: "https://hive.example".into(),
                    credential: "hive-synthetic".into(),
                    defaults: selected.clone(),
                    credentials: credentials(token),
                })
                .unwrap();
                manager.synchronize(Arc::new(Client)).wait().await;
                manager.status().json()
            } else {
                let manager = McpManager::new(McpSettings {
                    home: home.clone(),
                    host_id: "fixture".into(),
                    hive_url: "https://hive.example".into(),
                    credential: "hive-synthetic".into(),
                    defaults: selected.clone(),
                    credentials: credentials(token),
                });
                manager.synchronize(Arc::new(Client)).wait().await;
                manager.status().json()
            };
            assert_eq!(status.get("state"), Some(&Json::from("applied")));
            let bytes = fs::read_to_string(&target).unwrap();
            assert!(bytes.contains(&format!("Bearer {token}")));
            assert!(!bytes.contains("hive-synthetic"));
            if token == "synthetic-rotated" {
                assert!(!bytes.contains("synthetic-first"));
            }
            let public = stringify(&status, usize::MAX).unwrap();
            assert!(!public.contains(token));
            assert!(!public.contains("hive-synthetic"));
            let version = status.get("appliedVersion").unwrap().clone();
            assert_ne!(previous.as_ref(), Some(&version));
            previous = Some(version);
        }
    }
}
