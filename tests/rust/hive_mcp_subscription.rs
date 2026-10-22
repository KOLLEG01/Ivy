use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_hive::{
    mcp_inbound,
    mcp_subscription::{self as subscription, Admission},
};

fn cases() -> Vec<IvyJson> {
    let reference = super::mcp_inbound::reference();
    let template = reference
        .get("cases")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row.get("name") == Some(&"modern-ping".into()))
        .unwrap()
        .clone();
    let mut cases = vec![];
    let add = |cases: &mut Vec<IvyJson>,
               name: String,
               filter: Option<IvyJson>,
               ending: &str,
               id: IvyJson,
               pre_abort: bool| {
        let mut row = template.clone();
        row.insert("name", name.into());
        super::mcp_inbound::set(&mut row, &["body", "id"], id);
        super::mcp_inbound::set(&mut row, &["body", "method"], "subscriptions/listen".into());
        super::mcp_inbound::set(
            &mut row,
            &["headers", "mcp-method"],
            "subscriptions/listen".into(),
        );
        if let Some(filter) = filter {
            super::mcp_inbound::set(&mut row, &["body", "params", "notifications"], filter);
        }
        row.insert(
            "actions",
            IvyJson::Array(
                [
                    "tick", "publish", "tick", ending, "tick", "publish", "close", "abort",
                    "cancel",
                ]
                .map(IvyJson::from)
                .to_vec(),
            ),
        );
        row.insert("preAbort", pre_abort.into());
        cases.push(row);
    };
    let valid = [
        r#"{}"#,
        r#"{"toolsListChanged":true}"#,
        r#"{"toolsListChanged":false}"#,
        r#"{"promptsListChanged":true}"#,
        r#"{"resourcesListChanged":true}"#,
        r#"{"resourceSubscriptions":[]}"#,
        r#"{"resourceSubscriptions":["fixture://one","fixture://one",""]}"#,
        r#"{"toolsListChanged":true,"promptsListChanged":true,"resourcesListChanged":true,"resourceSubscriptions":["\ud800"]}"#,
        r#"{"unknown":{"nested":true},"__proto__":{"toolsListChanged":true}}"#,
    ];
    for (index, filter) in valid.iter().enumerate() {
        for ending in ["close", "abort", "cancel"] {
            add(
                &mut cases,
                format!("valid-{index}-{ending}"),
                Some(json(filter)),
                ending,
                "subscription".into(),
                false,
            );
        }
    }
    for (index, filter) in [
        None,
        Some("null"),
        Some("true"),
        Some("1"),
        Some(r#""bad""#),
        Some("[]"),
    ]
    .iter()
    .enumerate()
    {
        add(
            &mut cases,
            format!("invalid-filter-{index}"),
            filter.map(json),
            "close",
            "subscription".into(),
            false,
        );
    }
    for field in [
        "toolsListChanged",
        "promptsListChanged",
        "resourcesListChanged",
    ] {
        for (index, value) in ["null", "0", r#""true""#, "[]", "{}"].iter().enumerate() {
            add(
                &mut cases,
                format!("invalid-{field}-{index}"),
                Some(obj([(field, json(value))])),
                "close",
                "subscription".into(),
                false,
            );
        }
    }
    for (index, value) in [
        "null", "true", "1", r#""uri""#, "{}", "[null]", "[1]", "[{}]",
    ]
    .iter()
    .enumerate()
    {
        add(
            &mut cases,
            format!("invalid-uris-{index}"),
            Some(obj([("resourceSubscriptions", json(value))])),
            "close",
            "subscription".into(),
            false,
        );
    }
    for (index, id) in [
        "0",
        "-1",
        "9007199254740991",
        r#""""#,
        r#""unicode-😀""#,
        r#""\ud800""#,
    ]
    .iter()
    .enumerate()
    {
        for ending in ["close", "abort"] {
            add(
                &mut cases,
                format!("id-{index}-{ending}"),
                Some(json("{}")),
                ending,
                json(id),
                false,
            );
        }
    }
    for (index, id) in ["subscription".into(), IvyJson::Number(0.0)]
        .into_iter()
        .enumerate()
    {
        add(
            &mut cases,
            format!("pre-aborted-{index}"),
            Some(json("{}")),
            "close",
            id,
            true,
        );
    }
    for (index, accept) in [None, Some("application/json"), Some("text/plain")]
        .into_iter()
        .enumerate()
    {
        add(
            &mut cases,
            format!("accept-{index}"),
            Some(json("{}")),
            "close",
            "subscription".into(),
            false,
        );
        let row = cases.last_mut().unwrap();
        if let Some(value) = accept {
            super::mcp_inbound::set(row, &["headers", "accept"], value.into());
        } else {
            row.get_mut("headers").unwrap().remove("accept");
        }
    }
    cases
}
fn snapshot(action: &IvyJson, chunks: &[IvyJson], closed: bool) -> IvyJson {
    obj([
        ("action", action.clone()),
        ("chunks", IvyJson::Array(chunks.to_vec())),
        ("closed", closed.into()),
    ])
}
fn execute(row: &IvyJson, info: &IvyJson) -> IvyJson {
    let request = super::mcp_inbound::at(row);
    let route = mcp_inbound::classify(&request);
    assert!(matches!(
        route,
        mcp_inbound::Route::Modern {
            notification: false,
            ..
        }
    ));
    assert!(
        mcp_inbound::entry_rejection(&request, &route)
            .unwrap()
            .is_none()
    );
    let mut chunks = vec![];
    let mut snapshots = vec![];
    let (status, headers) = match subscription::admit(row.get("body").unwrap(), info).unwrap() {
        Admission::Rejected(reply) => {
            chunks.push(STANDARD.encode(reply.body).into());
            snapshots.push(snapshot(&"rejected".into(), &chunks, true));
            (reply.status, reply.headers)
        }
        Admission::Stream(opening, mut session) => {
            chunks.push(STANDARD.encode(opening.first_frame).into());
            if row.get("preAbort") == Some(&true.into()) {
                assert!(session.close(false).unwrap().is_none());
            }
            snapshots.push(snapshot(&"open".into(), &chunks, session.closed()));
            for action in row.get("actions").unwrap().as_array().unwrap() {
                let frame = match action.as_str().unwrap().as_str() {
                    "tick" => session.tick().map(str::to_owned),
                    "close" => session.close(true).unwrap(),
                    "abort" | "cancel" => session.close(false).unwrap(),
                    "publish" => None, // Hive's honored filter is empty, even for all four bus changes.
                    _ => panic!("Unexpected fixture action"),
                };
                if let Some(frame) = frame {
                    chunks.push(STANDARD.encode(frame).into());
                }
                snapshots.push(snapshot(action, &chunks, session.closed()));
            }
            assert!(session.closed());
            (opening.status, opening.headers)
        }
    };
    let bytes: Vec<u8> = chunks
        .iter()
        .flat_map(|v| STANDARD.decode(v.as_str().unwrap()).unwrap())
        .collect();
    obj([
        ("name", row.get("name").unwrap().clone()),
        ("status", IvyJson::Number(status.into())),
        ("headers", headers),
        ("snapshots", IvyJson::Array(snapshots)),
        ("bodyBase64", STANDARD.encode(bytes).into()),
    ])
}
fn save(suffix: &str, value: &IvyJson) {
    if let Some(prefix) = std::env::var_os("IVY_HIVE_MCP_SUBSCRIPTION_CORPUS") {
        std::fs::write(
            format!("{}-{suffix}.json", prefix.to_string_lossy()),
            encode(value),
        )
        .unwrap();
    }
}
#[test]
fn modern_subscription_frames_and_lifecycle_match_original_sdk() {
    let cases = cases();
    let source = super::mcp_protocol::oracle(&obj([
        ("mode", "subscription-lifecycle".into()),
        ("cases", IvyJson::Array(cases.clone())),
    ]));
    save("source", &source);
    let native: Vec<_> = cases
        .iter()
        .map(|row| execute(row, source.get("serverInfo").unwrap()))
        .collect();
    save(
        "comparison",
        &obj([
            ("cases", IvyJson::Array(cases)),
            ("source", source.clone()),
            ("native", IvyJson::Array(native.clone())),
        ]),
    );
    for (original, native) in source
        .get("observations")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .zip(native)
    {
        let mut projected = original.clone();
        projected.remove("trace");
        for snapshot in projected
            .get_mut("snapshots")
            .unwrap()
            .as_array_mut()
            .unwrap()
        {
            snapshot.remove("listeners");
            snapshot.remove("timers");
        }
        assert_eq!(
            projected,
            native,
            "{}",
            encode(original.get("name").unwrap())
        );
        let trace = original.get("trace").unwrap().as_array().unwrap();
        assert_eq!(trace[0].get("kind"), Some(&"factory".into()));
        assert_eq!(trace[1].get("kind"), Some(&"product-close".into()));
        for event in trace
            .iter()
            .filter(|e| e.get("kind") == Some(&"timer-start".into()))
        {
            assert_eq!(
                event.get("delay"),
                Some(&IvyJson::Number(subscription::KEEP_ALIVE_MS as f64))
            );
        }
        let last = original
            .get("snapshots")
            .unwrap()
            .as_array()
            .unwrap()
            .last()
            .unwrap();
        assert_eq!(last.get("listeners"), Some(&IvyJson::Number(0.0)));
        assert_eq!(last.get("timers"), Some(&IvyJson::Number(0.0)));
    }
}
