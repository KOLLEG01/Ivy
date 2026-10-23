use super::*;
use crate::service_test_support::{equal, observed};
use ivy_host_runtime::node_path::Context;
fn cases() -> IvyJson {
    ivy_contracts::json::parse(
        include_str!("../fixtures/phone-settings-cases.json"),
        256 * 1024,
    )
    .unwrap()
}
#[test]
fn service_policy_paths_voice_bounds_and_audio_requirements() {
    // Configuration admission is lexical; exercise Windows rules on every host.
    let home = JsString::from("C:\\phone-fixture");
    let paths = Context {
        style: node_path::Style::Windows,
        cwd: home.clone(),
    };
    let environment = std::collections::BTreeMap::new();
    let context = HomeContext {
        paths: &paths,
        environment: &environment,
        user_home: Some(&home),
        cwd: &paths.cwd,
        account_home: &home,
    };
    let cases = cases();
    for row in cases.get("cases").unwrap().as_array().unwrap() {
        let value = row.get("value").unwrap();
        let result = phone_settings(value, &context);
        let name = row.get("name").unwrap().as_str().unwrap();
        if row.get("ok") == Some(&true.into()) {
            equal(&result.unwrap(), value, &name);
        } else {
            assert_eq!(
                result.unwrap_err().code_js(),
                row.get("code").unwrap().as_js_string().unwrap().clone(),
                "{name}"
            );
        }
    }
    let base = cases.get("cases").unwrap().as_array().unwrap()[0]
        .get("value")
        .unwrap();
    for (prompt, valid) in [
        (JsString::from("é".repeat(16000)), true),
        (JsString::from("é".repeat(16001)), false),
        (JsString(vec![0xd800; 100]), true),
    ] {
        let mut value = base.clone();
        value.insert("incomingInitialPrompt", prompt.into());
        assert_eq!(phone_settings(&value, &context).is_ok(), valid);
    }
}

#[tokio::test]
async fn protected_sections_refuse_missing_malformed_and_oversized_files() {
    let cases = cases();
    let root = std::env::temp_dir().join(format!("ivy-phone-settings-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    for (index, row) in cases
        .get("files")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let path = root.join(format!("file{index}.json"));
        if row.get("oversized") == Some(&true.into()) {
            std::fs::write(&path, " ".repeat(128 * 1024 + 1)).unwrap();
        } else if let Some(raw) = row.get("raw") {
            std::fs::write(&path, raw.as_str().unwrap()).unwrap();
        } else if let Some(document) = row.get("document") {
            std::fs::write(
                &path,
                ivy_contracts::json::stringify(document, usize::MAX).unwrap(),
            )
            .unwrap();
        }
        let path = if row.get("none") == Some(&true.into()) {
            None
        } else if row.get("relative") == Some(&true.into()) {
            Some("relative.json".into())
        } else {
            Some(node_path::from_native(&path))
        };
        let result = if row.get("kind") == Some(&"credentials".into()) {
            phone_credentials(path).await
        } else {
            let section = match row.get("section").unwrap().as_str().unwrap().as_str() {
                "sip" => PhoneConfigurationSection::Sip,
                "access" => PhoneConfigurationSection::Access,
                "speech" => PhoneConfigurationSection::Speech,
                _ => panic!("Unknown section"),
            };
            phone_configuration_section(path.unwrap(), section).await
        };
        equal(
            &observed(result),
            row.get("expected").unwrap(),
            &row.get("name").unwrap().as_str().unwrap(),
        );
    }
    assert!(root.starts_with(std::env::temp_dir()));
    assert!(
        root.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("ivy-phone-settings-")
    );
    std::fs::remove_dir_all(root).unwrap();
}
