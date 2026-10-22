use ivy_cli::output::CliResult;
use ivy_contracts::{IvyError, IvyJson, JsString, json::parse};

#[test]
fn cli_failure_projection_retains_raw_outcomes_and_existing_exit_policy() {
    for value in [
        r#"["unknown"]"#,
        r#"["completed"]"#,
        r#"[["not_executed"]]"#,
        r#""not_executed""#,
        r#""unknown""#,
        r#""completed""#,
    ] {
        let raw = parse(value, 100_000).unwrap();
        for (code, exit) in [
            ("remote", 1),
            ("invalid_arguments", 2),
            ("needs_attention", 3),
        ] {
            let mut error = IvyError::new_js(code.into(), JsString(vec![0xd800; 2050]))
                .with_provider_outcome(raw.clone());
            for details in [
                None,
                Some(IvyJson::Null),
                Some(IvyJson::String(JsString(vec![0xdfff]))),
            ] {
                error.details = details.clone();
                let result = CliResult::failure(&error, Some(&JsString(vec![0xdfff, 65])));
                assert_eq!(result.exit_code, exit);
                assert_eq!(result.output.get("code"), Some(&IvyJson::from(code)));
                assert_eq!(
                    result.output.get("deploymentId"),
                    Some(&IvyJson::String(JsString(vec![0xdfff, 65])))
                );
                let data = result.output.get("data").unwrap();
                assert_eq!(data.get("outcome"), Some(&raw));
                assert_eq!(
                    data.get("message"),
                    Some(&IvyJson::String(error.message_js()))
                );
                assert_eq!(data.get("details"), details.as_ref());
            }
        }
    }
}
