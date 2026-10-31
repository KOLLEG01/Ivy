use super::*;
use crate::retention::graph_tests::{LIVE, PEAK};
use std::sync::atomic::Ordering;

fn previous_projection(body: &IvyJson, modern: bool) -> Result<IvyJson, Vec<Issue>> {
    let mut lifted = body.clone();
    if let Some(params) = body.get("params") {
        let mut params = params.clone();
        if let Some(meta) = params.get("_meta") {
            let mut meta = meta.clone();
            let mut found = false;
            for key in [
                mcp_inbound::VERSION_META,
                mcp_inbound::INFO_META,
                mcp_inbound::CAPABILITIES_META,
                mcp_inbound::LOG_META,
            ] {
                found |= meta.remove(key).is_some();
            }
            if found {
                if meta.as_object().is_some_and(|v| v.is_empty()) {
                    params.remove("_meta");
                } else {
                    params.insert("_meta", meta);
                }
            }
        }
        params.remove("inputResponses");
        params.remove("requestState");
        lifted.insert("params", params);
    }
    parse(
        if modern {
            &schemas().modern_call
        } else {
            &schemas().legacy_call
        },
        &lifted,
    )
}

fn call(params: IvyJson) -> IvyJson {
    IvyJson::object([
        ("jsonrpc", "2.0".into()),
        ("id", "request".into()),
        ("method", "tools/call".into()),
        ("params", params),
    ])
}

#[test]
fn projection_keeps_metadata_presence_opaque_utf16_and_issue_order() {
    let json = |text| ivy_contracts::json::parse(text, usize::MAX).unwrap();
    let metadata = [
        json("{}"),
        json("null"),
        json("[]"),
        json(r#"{"io.modelcontextprotocol/protocolVersion":true}"#),
        json(
            r#"{"io.modelcontextprotocol/protocolVersion":"v","io.modelcontextprotocol/clientInfo":{},"io.modelcontextprotocol/clientCapabilities":{},"io.modelcontextprotocol/logLevel":"debug"}"#,
        ),
        json(
            r#"{"progressToken":"\ud800","io.modelcontextprotocol/protocolVersion":{},"\ud801":{"__proto__":true,"number":-0}}"#,
        ),
        json(
            r#"{"io.modelcontextprotocol/protocolVersion\u0000":true,"io.modelcontextprotocol/related-task":{"taskId":false,"extra":1},"progressToken":[]}"#,
        ),
    ];
    for modern in [false, true] {
        for meta in std::iter::once(None).chain(metadata.iter().map(Some)) {
            for valid in [false, true] {
                let mut params = IvyJson::object([
                    (
                        "name",
                        if valid {
                            "fixture".into()
                        } else {
                            false.into()
                        },
                    ),
                    (
                        "arguments",
                        if valid {
                            json(
                                r#"{"_meta":{"io.modelcontextprotocol/protocolVersion":"\ud800"},"inputResponses":[],"requestState":{},"__proto__":true}"#,
                            )
                        } else {
                            json("[]")
                        },
                    ),
                    ("inputResponses", true.into()),
                    ("requestState", json(r#"{"opaque":true}"#)),
                ]);
                if let Some(meta) = meta {
                    params.insert("_meta", meta.clone());
                }
                let body = call(params);
                if mcp_protocol::message_kind(&body) != Some(mcp_protocol::MessageKind::Request) {
                    assert!(request(&body, modern).is_none());
                    continue;
                }
                let expected = previous_projection(&body, modern);
                let actual = request(&body, modern).unwrap();
                assert_eq!(actual, expected);
                if let (Err(actual), Err(expected)) = (actual, expected) {
                    assert_eq!(diagnostic(&actual).unwrap(), diagnostic(&expected).unwrap());
                }
            }
        }
    }
    for modern in [false, true] {
        for (meta, retained) in [
            (json("{}"), true),
            (
                json(r#"{"io.modelcontextprotocol/protocolVersion":"v"}"#),
                false,
            ),
        ] {
            let body = call(IvyJson::object([
                ("name", "fixture".into()),
                ("_meta", meta),
            ]));
            assert_eq!(
                request(&body, modern)
                    .unwrap()
                    .unwrap()
                    .required("params")
                    .unwrap()
                    .get("_meta")
                    .is_some(),
                retained
            );
        }
    }
}

#[test]
fn projection_allocates_only_the_retained_payload() {
    const NAME: &str =
        "mcp_parameters::ownership_tests::projection_allocates_only_the_retained_payload";
    const CHILD: &str = "IVY_MCP_PARAMETER_PROJECTION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        return;
    }
    const PAYLOAD_BYTES: isize = 2 * 1024 * 1024;
    schemas();
    for modern in [false, true] {
        for slot in ["arguments", "metadata", "wire-metadata", "retry", "body"] {
            let payload = IvyJson::from("x".repeat(1024 * 1024));
            let mut params = IvyJson::object([("name", "fixture".into())]);
            let mut extra = None;
            match slot {
                "arguments" => params.insert("arguments", IvyJson::object([("payload", payload)])),
                "metadata" => params.insert("_meta", IvyJson::object([("opaque", payload)])),
                "wire-metadata" => params.insert(
                    "_meta",
                    IvyJson::object([(
                        mcp_inbound::CAPABILITIES_META,
                        IvyJson::object([(
                            "experimental",
                            IvyJson::object([("fixture", payload)]),
                        )]),
                    )]),
                ),
                "retry" => params.insert("inputResponses", IvyJson::object([("fixture", payload)])),
                "body" => extra = Some(payload),
                _ => unreachable!(),
            }
            let mut body = call(params);
            if let Some(extra) = extra {
                body.insert("id", extra);
            }
            let mut expected = None;
            let mut original_peak = 0;
            for original in [true, false] {
                let before = LIVE.load(Ordering::Relaxed);
                PEAK.store(before, Ordering::Relaxed);
                let result = if original {
                    previous_projection(&body, modern)
                } else {
                    request(&body, modern).unwrap()
                }
                .unwrap();
                let peak = PEAK.load(Ordering::Relaxed) - before;
                eprintln!(
                    "MCP parameter Rust allocation peak: {peak} bytes; original={original}; modern={modern}; slot={slot}"
                );
                if original {
                    original_peak = peak;
                    expected = Some(result);
                } else {
                    let retained = matches!(slot, "arguments" | "metadata");
                    let budget = if retained { PAYLOAD_BYTES } else { 0 };
                    assert!(
                        peak < budget + 64 * 1024,
                        "Projection copied discarded data or retained payload more than once: {peak}"
                    );
                    assert!(original_peak > peak + PAYLOAD_BYTES / 2);
                    assert_eq!(Some(result), expected);
                }
            }
        }
    }
}
