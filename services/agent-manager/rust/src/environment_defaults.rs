//! Project-owned generated defaults; URL mechanics stay in the shared contracts.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    agent_schema::validate_agent,
    json::parse,
    whatwg_url::{parse_url_with_base, url_components},
};

pub fn load_environment_defaults(
    _artifact_root: &JsString,
    public_base_url: &JsString,
) -> IvyResult<IvyJson> {
    let mut value = parse(
        include_str!("environment-defaults.json"),
        usize::MAX,
    )?;
    let base = url_components(public_base_url)?;
    let pathname = base.pathname.strip_suffix('/').unwrap_or(&base.pathname);
    let mut mcp = value.required("mcp")?.clone();
    let mut servers = mcp
        .required("servers")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .to_vec();
    for server in &mut servers {
        let suffix = if server.get("name") == Some(&IvyJson::from("ivy_dev")) {
            "/mcp-dev"
        } else {
            "/mcp"
        };
        let url = parse_url_with_base(
            &JsString::from(format!("{pathname}{suffix}")),
            &JsString::from(base.origin.as_str()),
        )?;
        server.insert("url", url.href.into());
    }
    mcp.insert("servers", IvyJson::Array(servers));
    value.insert("mcp", mcp);
    validate_agent("EnvironmentDefaults", &value)?;
    Ok(value)
}
