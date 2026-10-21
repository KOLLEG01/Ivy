use super::*;

#[test]
fn ui_windows_namespace_preserves_actual_staged_promoted_read_and_collected_asset_names() {
    let names = ["tail..", "space. ", "folder. /nested..", "é. ", "💡.."];
    let assets: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(i, name)| asset(name, "text/plain", [i as u8, 0, 255]))
        .collect();
    let mut steps: Vec<_> = assets
        .iter()
        .enumerate()
        .map(|(i, asset)| stage(asset.clone(), [i as u8, 0, 255]))
        .collect();
    let published = push(&mut steps, promote(assets.clone()));
    for asset in &assets {
        for method in ["available", "check", "read"] {
            steps.push(file_action(method, asset.clone()));
        }
        steps.push(action(
            "path",
            json!({"uiId":"sample-ui","releaseId":"r1","path":asset["path"]}),
        ));
    }
    steps.push(fixture_step(vec![mtime("private/sample-ui/r1", 0.0)]));
    let collected = push(
        &mut steps,
        action("collect", json!({"uiId":"sample-ui","retained":[]})),
    );
    let result = compare(&steps);
    for index in 0..steps.len() {
        success(&result, index);
    }
    for name in names {
        assert!(has_file(
            &result,
            published,
            &format!("private/sample-ui/r1/{name}")
        ));
        assert!(!has_file(
            &result,
            collected,
            &format!("private/sample-ui/r1/{name}")
        ));
    }
}

#[test]
fn ui_windows_namespace_installs_actual_bundle_files_and_cleans_failed_staging_before_retry() {
    let plain = asset("tail..", "text/plain", b"one");
    let structured = asset("folder. /entry. ", "application/json", br#"{"a":1,"z":2}"#);
    let assets = vec![plain.clone(), structured.clone()];
    let mut steps = vec![fixture_step(vec![
        write("bundle/tail..", b"one"),
        write("bundle/folder. /entry. ", br#"{ "z": 2, "a": 1 }"#),
    ])];
    let install = |release: &str| {
        action(
            "install",
            json!({"uiId":"sample-ui","releaseId":release,"assets":assets}),
        )
    };
    steps.push(install("r1"));
    steps.push(file_action("read", structured));
    steps.push(fixture_step(vec![remove("bundle/tail..")]));
    let failed = push(&mut steps, install("r2"));
    steps.push(fixture_step(vec![write("bundle/tail..", b"one")]));
    let retry = push(&mut steps, install("r2"));
    let result = compare(&steps);
    for index in 0..steps.len() {
        if index == failed {
            error(&result, index, "internal_error");
        } else {
            success(&result, index);
        }
    }
    assert!(has_file(&result, retry, "private/sample-ui/r2/tail.."));
    assert!(has_file(
        &result,
        retry,
        "private/sample-ui/r2/folder. /entry. "
    ));
    assert!(
        !result["rows"][failed]["fileTree"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"].as_str().unwrap().contains("/.incoming/"))
    );
}

#[test]
fn ui_windows_namespace_root_keeps_logical_paths_without_creating_trimmed_aliases() {
    let root = Root::new();
    fixture(&root.0, &json!([mkdir("native. "), mkdir("reference. ")]));
    let descriptor = asset("tail..", "text/plain", b"one");
    let steps = vec![
        stage(descriptor.clone(), b"one"),
        promote(vec![descriptor.clone()]),
        file_action("read", descriptor),
        action(
            "path",
            json!({"uiId":"sample-ui","releaseId":"r1","path":"tail.."}),
        ),
    ];
    let result = compare_at(
        &root.0.join("native. ").join("db.sqlite"),
        &root.0.join("reference. ").join("db.sqlite"),
        &steps,
    );
    for index in 0..steps.len() {
        success(&result, index);
    }
    assert!(!root.0.join("native").exists());
    assert!(!root.0.join("reference").exists());
}
