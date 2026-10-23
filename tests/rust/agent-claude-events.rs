use ivy_agent_manager::claude::{
    sdk_events::SdkTurn,
    workflow::{EventSink, WorkflowFiles},
};
use ivy_contracts::{IvyJson, json::parse};
use ivy_host_runtime::node_path;
use std::sync::{Arc, Mutex};

fn json(source: &str) -> IvyJson {
    parse(source, 65536).unwrap()
}

#[test]
fn native_quota_windows_parse_reset_times_retain_storage_and_expire() {
    use ivy_agent_manager::claude::rate_limits::RateLimitStore;
    use std::fs;
    struct Home(std::path::PathBuf);
    impl Drop for Home {
        fn drop(&mut self) {
            assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
            assert!(
                self.0
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("ivy-agent-rates-")
            );
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let home = Home(std::env::temp_dir().join(format!("ivy-agent-rates-{}", uuid::Uuid::new_v4())));
    fs::create_dir(&home.0).unwrap();
    let mut store = RateLimitStore::open(&home.0);
    assert!(!store.usage_due(10000.0, 60000.0));
    store.record_event(&json(r#"{"rateLimitType":"five_hour","utilization":0.445,"resetsAt":20,"status":"rejected"}"#), 10000.0);
    let state = store.read(10.0).unwrap();
    let current = state.get("rateLimits").unwrap();
    assert_eq!(
        current.get("rateLimitReachedType"),
        Some(&"rate_limit_reached".into())
    );
    assert_eq!(
        current.get("primary"),
        Some(&json(
            r#"{"windowDurationMins":300,"usedPercent":45,"resetsAt":20}"#
        ))
    );
    let saved = fs::read(home.0.join("rate-limits.json")).unwrap();
    store.record_event(
        &json(r#"{"rateLimitType":"overage","utilization":1}"#),
        10000.0,
    );
    assert_eq!(fs::read(home.0.join("rate-limits.json")).unwrap(), saved);
    store.record_usage(&json(r#"{"five_hour":{"utilization":-5,"resets_at":"1970-01-01T01:01:00+01:00"},"seven_day":{"utilization":110,"resets_at":120},"seven_day_opus":{"utilization":55.5,"resets_at":"invalid"},"model_scoped":[{"display_name":"é","utilization":12.5,"resets_at":-0.5}]}"#), 20000.0);
    assert!(!store.usage_due(79999.0, 60000.0));
    assert!(store.usage_due(80000.0, 60000.0));
    let state = store.read(0.0).unwrap();
    let current = state.get("rateLimits").unwrap();
    assert_eq!(
        current.get("primary"),
        Some(&json(
            r#"{"windowDurationMins":300,"usedPercent":0,"resetsAt":60}"#
        ))
    );
    assert_eq!(
        current.get("secondary"),
        Some(&json(
            r#"{"windowDurationMins":10080,"usedPercent":100,"resetsAt":120}"#
        ))
    );
    let buckets = state.get("rateLimitsByLimitId").unwrap();
    assert_eq!(
        buckets.get("seven_day_opus").unwrap().get("primary"),
        Some(&json(
            r#"{"windowDurationMins":10080,"usedPercent":56,"resetsAt":null}"#
        ))
    );
    let reopened = RateLimitStore::open(&home.0);
    assert_eq!(reopened.read(0.0).unwrap(), state);
    assert!(reopened.usage_due(60000.0, 60000.0));
    let expired = store.read(120.0).unwrap();
    let current = expired.get("rateLimits").unwrap();
    assert_eq!(
        current.get("primary"),
        Some(&json(
            r#"{"windowDurationMins":300,"usedPercent":0,"resetsAt":null}"#
        ))
    );
    assert_eq!(
        current.get("secondary"),
        Some(&json(
            r#"{"windowDurationMins":10080,"usedPercent":0,"resetsAt":null}"#
        ))
    );
    for (date, expected) in [
        ("Thu, 01 Jan 1970 00:01:00 GMT", 60.0),
        ("1970-01-02", 86400.0),
    ] {
        store.record_usage(
            &json(&format!(
                r#"{{"five_hour":{{"utilization":10,"resets_at":"{date}"}}}}"#
            )),
            21000.0,
        );
        assert_eq!(
            store
                .read(0.0)
                .unwrap()
                .get("rateLimits")
                .unwrap()
                .get("primary")
                .unwrap()
                .get("resetsAt"),
            Some(&IvyJson::Number(expected))
        );
    }
    fs::remove_file(home.0.join("rate-limits.json")).unwrap();
    fs::create_dir(home.0.join("rate-limits.json")).unwrap();
    store.record_event(
        &json(r#"{"rateLimitType":"five_hour","utilization":0.3,"resetsAt":200}"#),
        130000.0,
    );
    assert_eq!(
        store
            .read(130.0)
            .unwrap()
            .get("rateLimits")
            .unwrap()
            .get("primary"),
        Some(&json(
            r#"{"windowDurationMins":300,"usedPercent":30,"resetsAt":200}"#
        ))
    );
}

#[tokio::test]
async fn native_sdk_streams_merge_text_and_thinking_without_repeating_completed_blocks() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let target = events.clone();
    let sink: EventSink = Arc::new(move |event| {
        let target = target.clone();
        Box::pin(async move {
            target.lock().unwrap().push(event);
            Ok(())
        })
    });
    let home = node_path::from_native(&std::env::temp_dir().join("ivy-sdk-event-fixture"));
    let mut context = json(r#"{"threadId":"thread","turnId":"turn"}"#);
    context.insert("cwd", IvyJson::String(home.clone()));
    let files = WorkflowFiles {
        realpath: Arc::new(|_| panic!("ordinary stream does not read workflow paths")),
        read_regular: Arc::new(|_, _, _| panic!("ordinary stream does not read workflow journals")),
    };
    let mut turn = SdkTurn::new(context, &home, &json("{}"), files, sink);
    for message in [
        r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"message"}}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello "}}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"thinking_delta","thinking":"Plan "}}}"#,
        r#"{"type":"assistant","message":{"id":"message","content":[{"type":"thinking","thinking":"Plan done"},{"type":"text","text":"Hello world"}]}}"#,
    ] {
        turn.message(&json(message)).await.unwrap();
    }
    let before = events.lock().unwrap().clone();
    let deltas = |kind: &str| {
        before
            .iter()
            .filter(|event| event.get("type") == Some(&kind.into()))
            .map(|event| event.get("delta").unwrap().as_str().unwrap())
            .collect::<String>()
    };
    assert_eq!(deltas("text_delta"), "Hello world");
    assert_eq!(deltas("reasoning_delta"), "Plan done");
    for index in ["-1", "0.5", "\"0\"", "null"] {
        let message = format!(
            r#"{{"type":"stream_event","event":{{"type":"content_block_delta","index":{index},"delta":{{"type":"text_delta","text":"invalid"}}}}}}"#
        );
        let error = turn.message(&json(&message)).await.unwrap_err();
        assert_eq!(error.name, "invalid_data");
    }
    assert_eq!(*events.lock().unwrap(), before);
    for message in [
        r#"{"type":"system","subtype":"task_started","task_type":"local_workflow","task_id":"workflow"}"#,
        r#"{"type":"system","subtype":"task_notification","task_id":"workflow","status":"completed","summary":"Finished","usage":{"total_tokens":10,"tool_uses":2,"duration_ms":30}}"#,
    ] {
        turn.message(&json(message)).await.unwrap();
    }
    assert!(events.lock().unwrap().iter().any(|event| {
        event.get("type") == Some(&"tool_result".into())
            && event.get("content")
                == Some(
                    &"Finished\n<usage>total_tokens: 10\ntool_uses: 2\nduration_ms: 30</usage>"
                        .into(),
                )
    }));
    for (reset, expected) in [
        ("1700000000.123", " Resets at 2023-11-14T22:13:20.123Z."),
        ("-0.5", " Resets at 1969-12-31T23:59:59.500Z."),
        ("1e30", ""),
    ] {
        let message = format!(
            r#"{{"type":"rate_limit","rate_limit_info":{{"status":"allowed_warning","rateLimitType":"five_hour","utilization":0.805,"resetsAt":{reset}}}}}"#
        );
        turn.message(&json(&message)).await.unwrap();
        assert!(events.lock().unwrap().iter().any(|event| {
            event.get("type") == Some(&"notice".into())
                && event.get("message")
                    == Some(
                        &format!(
                            "Claude usage is nearing the five hour limit (81% used).{expected}"
                        )
                        .into(),
                    )
        }));
    }
}
