use ivy_contracts::{
    agent_schema::validate_agent,
    automation_schema::validate_automation,
    chat_schema::validate_chat,
    component_schema::validate_component,
    core_schema::{validate_host, validate_transport},
    json::parse,
    secretary_schema::validate_secretary,
    wire_schema::validate_shared,
};

#[test]
fn domain_contracts_match_typescript_admission_and_errors() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/domain-schemas-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.len() > 1000);
    let mut accepted = std::collections::HashMap::<String, usize>::new();
    for row in rows {
        let domain = row["domain"].as_str().unwrap();
        let name = row["name"].as_str().unwrap();
        let input = match row["shape"].as_str() {
            Some("deep") => "[".repeat(129) + "0" + &"]".repeat(129),
            Some("wide") => "[0".to_owned() + &",0".repeat(400000) + "]",
            Some("large") => "\"".to_owned() + &"x".repeat(32 * 1024 * 1024) + "\"",
            _ => row["input"].as_str().unwrap().to_owned(),
        };
        let value = parse(&input, usize::MAX).unwrap();
        let result = match domain {
            "agent" => validate_agent(name, &value),
            "host" => validate_host(name, &value),
            "shared" => validate_shared(name, &value),
            "transport" => validate_transport(name, &value),
            "automation" => validate_automation(name, &value),
            "chat" => validate_chat(name, &value),
            "secretary" => validate_secretary(name, &value),
            "component" => validate_component(&value),
            _ => unreachable!(),
        };
        let summary = format!("{domain} {name}: {} input bytes", input.len());
        assert_eq!(
            result.is_ok(),
            row["error"].is_null(),
            "{summary}: {result:?}"
        );
        if let Err(error) = result {
            assert_eq!(
                error.code,
                row["error"]["code"].as_str().unwrap(),
                "{summary}"
            );
            assert_eq!(
                error.message,
                row["error"]["message"].as_str().unwrap(),
                "{summary}"
            );
            assert_eq!(
                error.outcome.as_str(),
                row["error"]["outcome"].as_str().unwrap(),
                "{summary}"
            );
        } else {
            *accepted.entry(domain.into()).or_default() += 1;
        }
    }
    for domain in [
        "automation",
        "chat",
        "secretary",
        "component",
        "host",
        "agent",
        "shared",
        "transport",
    ] {
        assert!(
            accepted.get(domain).copied().unwrap_or(0) > 10,
            "Missing valid examples for {domain}"
        );
    }
}
