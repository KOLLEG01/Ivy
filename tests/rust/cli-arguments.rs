use ivy_cli::{arguments, output::CliResult};
use ivy_contracts::{IvyError, IvyJson, Outcome, json::parse};

fn json(text: &serde_json::Value) -> IvyJson {
    parse(text.as_str().unwrap(), usize::MAX).unwrap()
}
fn result(value: CliResult) -> IvyJson {
    IvyJson::object([
        ("exitCode", IvyJson::Number(value.exit_code as f64)),
        ("output", value.output),
    ])
}

#[test]
fn admission_and_failures_match_original_cli() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/cli-arguments-oracle.mjs",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let rows = cases["rows"].as_array().unwrap();
    assert!(rows.len() > 1600);
    for row in rows {
        let args = json(&row["args"])
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_js_string().unwrap().clone())
            .collect::<Vec<_>>();
        let env = json(&row["env"]);
        let default = json(&row["defaultPath"]);
        let mut calls = 0;
        let admitted = arguments::parse(&args).and_then(|args| {
            let path = args.config_path(env.as_js_string(), || {
                calls += 1;
                Ok(default.as_js_string().unwrap().clone())
            })?;
            Ok(IvyJson::object([
                ("command", args.command.into()),
                ("values", args.values),
                ("waitMs", IvyJson::Number(args.wait_ms)),
                ("configPath", IvyJson::String(path)),
            ]))
        });
        let actual = match admitted {
            Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
            Err(e) => IvyJson::object([
                ("ok", false.into()),
                ("error", e.to_wire()),
                ("result", result(CliResult::failure(&e, None))),
            ]),
        };
        assert_eq!(calls, row["calls"].as_u64().unwrap(), "{row}");
        assert_eq!(actual, json(&row["result"]), "{row}");
    }
    for row in cases["failures"].as_array().unwrap() {
        let code = json(&row["code"]);
        let message = json(&row["message"]);
        let mut error = IvyError::new_js(
            code.as_js_string().unwrap().clone(),
            message.as_js_string().unwrap().clone(),
        )
        .with_outcome(match row["outcome"].as_str().unwrap() {
            "completed" => Outcome::Completed,
            "unknown" => Outcome::Unknown,
            _ => Outcome::NotExecuted,
        });
        if !row["details"].is_null() {
            error = error.with_details(json(&row["details"]));
        }
        let id = json(&row["id"]);
        assert_eq!(
            result(CliResult::failure(&error, id.as_js_string())),
            json(&row["expected"]),
            "{row}"
        );
    }
    for row in cases["malformed"].as_array().unwrap() {
        let id = row["id"].as_str().map(Into::into);
        assert_eq!(
            result(CliResult::malformed_configuration(id.as_ref())),
            json(&row["expected"])
        );
    }
}

#[test]
fn output_validation_and_required_values_preserve_boundaries() {
    for wait in ["\u{feff}16\u{2029}", "\u{2028}\u{feff} 16 \u{feff}"] {
        let args =
            arguments::parse(&["deploy".into(), format!("--wait-ms={wait}").into()]).unwrap();
        assert_eq!(args.wait_ms, 16.0);
    }
    for wait in ["\u{0085}16", "16\u{0085}"] {
        assert_eq!(
            arguments::parse(&["deploy".into(), format!("--wait-ms={wait}").into()])
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
    }
    for prefix in ["0x", "0X", "0o", "0O", "0b", "0B"] {
        let wait = format!("{prefix}{}10", "0".repeat(1024));
        let args =
            arguments::parse(&["deploy".into(), format!("--wait-ms={wait}").into()]).unwrap();
        assert_eq!(
            args.wait_ms,
            match prefix {
                "0x" | "0X" => 16.0,
                "0o" | "0O" => 8.0,
                _ => 2.0,
            }
        );
        for suffix in ["f".repeat(32), "1".repeat(64), "+10".into(), "-10".into()] {
            let error = arguments::parse(&[
                "deploy".into(),
                format!("--wait-ms={prefix}{suffix}").into(),
            ])
            .unwrap_err();
            assert_eq!(error.code, "invalid_arguments");
        }
    }
    for (code, ok, exit) in [
        ("host_status", true, 0),
        ("deployment_failed", false, 1),
        ("deployment_needs_attention", true, 3),
        ("bootstrap_needs_attention", false, 3),
    ] {
        let output = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("ok", ok.into()),
            ("code", code.into()),
            ("data", IvyJson::Null),
        ]);
        assert_eq!(CliResult::success(output).unwrap().exit_code, exit);
    }
    assert!(CliResult::success(IvyJson::Null).is_err());
    let args = arguments::parse(&[
        "prepare".into(),
        "--component=".into(),
        "--source=\u{1f600}".into(),
    ])
    .unwrap();
    assert_eq!(args.required("source").unwrap(), &"😀".into());
    assert_eq!(
        args.required("component").unwrap_err().message,
        "Missing --component."
    );
    assert_eq!(
        args.required("instance").unwrap_err().message,
        "Missing --instance."
    );
}
