use ivy_chat_bridge::whatsapp::native_event::{
    EventProjection, MAX_NATIVE_EVENT_BYTES, MAX_NATIVE_EVENTS, NativeEvent, NativeEventKind,
    NativeEventQueue,
};
use ivy_contracts::{IvyJson as Json, JsString};

fn event(item: Json) -> Json {
    Json::object([
        ("method", "item/completed".into()),
        (
            "params",
            Json::object([
                ("threadId", "original-thread".into()),
                ("turnId", "original-turn".into()),
                ("item", item),
            ]),
        ),
    ])
}
fn projected(value: &Json) -> NativeEvent {
    match NativeEvent::project(value, MAX_NATIVE_EVENT_BYTES).unwrap() {
        EventProjection::Event(event) => event,
        _ => panic!("Expected an admitted native event"),
    }
}
fn agent(text: Json) -> Json {
    event(Json::object([
        ("type", "agentMessage".into()),
        ("id", "original-item".into()),
        ("text", text),
        ("phase", "commentary".into()),
    ]))
}

#[test]
fn projection_keeps_exact_identity_and_delegation_without_opaque_payloads() {
    let mut raw = agent(Json::String(JsString(vec![0x61, 0xd800, 0x62])));
    raw.insert("opaque", "x".repeat(2 * 1024 * 1024).into());
    let selected = projected(&raw);
    assert_eq!(selected.thread_id, Some("original-thread".into()));
    assert_eq!(selected.turn_id, Some("original-turn".into()));
    match selected.kind {
        NativeEventKind::Agent {
            id,
            text,
            commentary,
        } => {
            assert_eq!(id, Some("original-item".into()));
            assert_eq!(text, Some(JsString(vec![0x61, 0xd800, 0x62])));
            assert!(commentary);
        }
        _ => panic!("Wrong item kind"),
    }
    let selected = projected(&event(Json::object([
        ("type", "steeringUserMessage".into()),
        ("clientId", "original-client".into()),
        ("text", "x".repeat(2 * 1024 * 1024).into()),
        (
            "content",
            Json::Array(vec![Json::object([(
                "codexDelegation",
                Json::object([("sourceThreadId", "source-ä-😀".into())]),
            )])]),
        ),
    ])));
    assert!(selected.retained_bytes() < 1024);
    match selected.kind {
        NativeEventKind::User { client_id, source } => {
            assert_eq!(client_id, Some("original-client".into()));
            assert_eq!(source.unwrap(), Some("source-ä-😀".into()));
        }
        _ => panic!("Wrong user kind"),
    }
    let selected = projected(&event(Json::object([
        ("type", "userMessage".into()),
        (
            "content",
            Json::Array(vec![Json::object([(
                "codexDelegation",
                Json::object([("sourceThreadId", Json::Number(42.0))]),
            )])]),
        ),
    ])));
    assert!(matches!(
        selected.kind,
        NativeEventKind::User { source: Err(_), .. }
    ));
    assert!(
        NativeEvent::project(
            &event(Json::object([("type", Json::Bool(false))])),
            usize::MAX
        )
        .is_err()
    );
    assert!(matches!(
        NativeEvent::project(
            &event(Json::object([("type", "unknownProtocolItem".into())])),
            usize::MAX
        )
        .unwrap(),
        EventProjection::Ignore
    ));
}

#[test]
fn queue_bounds_count_and_actual_bytes_including_the_in_flight_delivery() {
    let small = agent("small".into());
    let mut queue = NativeEventQueue::default();
    let empty = queue.retained_bytes();
    for _ in 0..MAX_NATIVE_EVENTS {
        assert!(queue.push(projected(&small)));
    }
    assert!(!queue.push(projected(&small)));
    for _ in 0..MAX_NATIVE_EVENTS {
        let event = queue.pop().unwrap();
        queue.complete(&event);
    }
    assert!(queue.pop().is_none());
    assert_eq!(queue.retained_bytes(), empty);
    let large = agent("é😀".repeat(256 * 1024).into());
    assert!(queue.push(projected(&large)));
    let in_flight = queue.pop().unwrap();
    let held = queue.retained_bytes();
    assert!(held > 1024 * 1024);
    let mut admitted = 0;
    while queue.push(projected(&large)) {
        admitted += 1;
        assert!(queue.retained_bytes() <= MAX_NATIVE_EVENT_BYTES);
    }
    assert!(admitted > 0 && admitted < MAX_NATIVE_EVENTS);
    queue.clear();
    assert_eq!(queue.retained_bytes(), held);
    queue.complete(&in_flight);
    assert_eq!(queue.retained_bytes(), empty);
    assert!(matches!(
        NativeEvent::project(
            &agent("x".repeat(MAX_NATIVE_EVENT_BYTES).into()),
            MAX_NATIVE_EVENT_BYTES
        )
        .unwrap(),
        EventProjection::Recover
    ));
}
