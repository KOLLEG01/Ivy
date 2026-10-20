use ivy_contracts::{IvyJson, JsString, Outcome, json::*};
use std::{path::Path, process::Command};

fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.get(key).unwrap()
}
fn units(value: &IvyJson) -> JsString {
    JsString(
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap() as u16)
            .collect(),
    )
}

#[test]
fn raw_utf16_diagnostics_and_values_match_node() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = Command::new("node")
        .arg("tests/fixtures/json-diagnostics-oracle.mjs")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut count = 0;
    let mut summary = None;
    for line in output
        .stdout
        .split(|b| *b == b'\n')
        .filter(|s| !s.is_empty())
    {
        let row = parse(std::str::from_utf8(line).unwrap(), usize::MAX).unwrap();
        if row.get("summary").is_some() {
            summary = Some(field(&row, "summary").clone());
            continue;
        }
        let input = units(field(&row, "input"));
        let value =
            parse_with_js_diagnostics(&input, field(&row, "bytes").as_f64().unwrap() as usize);
        if let Some(message) = row.get("message") {
            let error = value.unwrap_err();
            let expected = units(message);
            assert_eq!(
                error.code,
                field(&row, "name").as_str().unwrap(),
                "input={:?}",
                input.0
            );
            assert_eq!(error.outcome, Outcome::NotExecuted);
            assert_eq!(
                error.wire_message.as_ref(),
                Some(&expected),
                "input={:?}",
                input.0
            );
            assert_eq!(error.message_js(), expected);
        } else {
            let value = value.unwrap_or_else(|e| panic!("input={:?}: {e:?}", input.0));
            assert_eq!(
                stringify(&value, usize::MAX).unwrap(),
                field(&row, "value").as_str().unwrap(),
                "input={:?}",
                input.0
            );
            if let Some(bits) = row.get("numberBits") {
                assert_eq!(
                    format!("{:016x}", value.as_f64().unwrap().to_bits()),
                    bits.as_str().unwrap()
                );
            }
        }
        count += 1;
    }
    let summary = summary.expect("complete original corpus");
    assert_eq!(field(&summary, "node").as_str().unwrap(), "24.18.0");
    assert_eq!(count, field(&summary, "count").as_f64().unwrap() as usize);
    assert!(count > 140_000);
    assert!(field(&summary, "failures").as_f64().unwrap() > 70_000.0);
    if let Some(path) = std::env::var_os("IVY_CLAUDE_JSON_PROOF") {
        std::fs::write(path, &output.stdout).unwrap();
    }
}

#[test]
fn byte_bound_precedes_syntax_and_legacy_errors_stay_unchanged() {
    for (input, bytes) in [
        (vec![34, 0xd800, 34], 5),
        (vec![34, 0xdc00, 34], 5),
        (vec![34, 0xd83d, 0xde00, 34], 6),
        (vec![0xd800], 3),
        (vec![34, 0xff, 34], 4),
    ] {
        let text = JsString(input);
        assert_eq!(
            parse_with_js_diagnostics(&text, bytes - 1)
                .unwrap_err()
                .code,
            "content_too_large"
        );
        assert_ne!(
            parse_with_js_diagnostics(&text, bytes)
                .err()
                .map(|e| e.code),
            Some("content_too_large".into())
        );
    }
    for (text, message) in [
        ("{", "Invalid JSON syntax."),
        ("[invalid]", "Invalid JSON syntax."),
        ("[nulx]", "Invalid JSON literal."),
        ("-", "Invalid JSON number."),
        ("true false", "Unexpected content after JSON value."),
        ("\"x", "Unterminated JSON string."),
        ("\"\\", "Invalid JSON escape."),
        ("\"\\uX000\"", "Invalid JSON Unicode escape."),
    ] {
        let error = parse(text, usize::MAX).unwrap_err();
        assert_eq!(error.code, "invalid_arguments");
        assert_eq!(error.message, message);
        assert_eq!(error.wire_message, None);
    }
    let parsed = parse_with_js_diagnostics(&JsString::from("{\"x\":1,\"x\":2}"), 100).unwrap();
    assert_eq!(parsed.get("x"), Some(&IvyJson::Number(2.0)));
}

#[test]
fn raw_utf16_deep_values_and_failure_cleanup_keep_an_iterative_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let depth = 50_000;
            let mut units = vec![91; depth];
            units.extend([34, 0xd800, 34]);
            units.extend(vec![93; depth]);
            let input = JsString(units);
            let value = parse_with_js_diagnostics(&input, usize::MAX).unwrap();
            drop(value);
            let mut broken = input.clone();
            broken.0.pop();
            let error = parse_with_js_diagnostics(&broken, usize::MAX).unwrap_err();
            assert_eq!(error.code, "SyntaxError");
            assert!(
                error
                    .message
                    .starts_with("Expected ',' or ']' after array element in JSON")
            );
        })
        .unwrap()
        .join()
        .unwrap();
}
