use ivy_agent_manager::environment_defaults::load_environment_defaults;
use ivy_contracts::{
    IvyJson, JsString,
    json::{canonical, parse},
};

#[test]
fn generated_defaults_preserve_public_endpoints_content_and_independent_documents() {
    let embedded = parse(
        include_str!("../../services/agent-manager/rust/src/environment-defaults.json"),
        usize::MAX,
    )
    .unwrap();
    for (base, ivy, development) in [
        (
            "https://example.invalid",
            "https://example.invalid/mcp",
            "https://example.invalid/mcp-dev",
        ),
        (
            "https://example.invalid/",
            "https://example.invalid/mcp",
            "https://example.invalid/mcp-dev",
        ),
        (
            "https://example.invalid/prefix",
            "https://example.invalid/prefix/mcp",
            "https://example.invalid/prefix/mcp-dev",
        ),
        (
            "https://example.invalid/prefix/",
            "https://example.invalid/prefix/mcp",
            "https://example.invalid/prefix/mcp-dev",
        ),
        (
            "http://127.0.0.1:4080/ivy/",
            "http://127.0.0.1:4080/ivy/mcp",
            "http://127.0.0.1:4080/ivy/mcp-dev",
        ),
    ] {
        let base = JsString::from(base);
        let mut expected = embedded.clone();
        let mut mcp = expected.required("mcp").unwrap().clone();
        let mut servers = mcp
            .required("servers")
            .unwrap()
            .as_array()
            .unwrap()
            .to_vec();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].get("name"), Some(&"ivy".into()));
        assert_eq!(servers[1].get("name"), Some(&"ivy_dev".into()));
        servers[0].insert("url", ivy.into());
        servers[1].insert("url", development.into());
        mcp.insert("servers", IvyJson::Array(servers));
        expected.insert("mcp", mcp);

        let mut first =
            load_environment_defaults(&"unused fixture artifact".into(), &base).unwrap();
        assert_eq!(
            canonical(&first, usize::MAX).unwrap(),
            canonical(&expected, usize::MAX).unwrap()
        );
        first.insert("instructions", IvyJson::Null);
        let second =
            load_environment_defaults(&"different fixture artifact".into(), &base).unwrap();
        assert_eq!(
            canonical(&second, usize::MAX).unwrap(),
            canonical(&expected, usize::MAX).unwrap()
        );
    }
}
