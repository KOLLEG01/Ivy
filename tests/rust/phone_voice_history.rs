use super::*;
use crate::voice_rpc::control_test_support::{Peer, Plan, fixture, hooks};
use std::time::Duration;

const THREAD: &str = "50000000-0000-4000-8000-000000000001";
const TURN: &str = "50000000-0000-4000-8000-000000000002";
const OPERATION: &str = "50000000-0000-4000-8000-000000000003";
fn object<const N: usize>(fields: [(&str, IvyJson); N]) -> IvyJson {
    IvyJson::object(fields)
}
fn turn(id: IvyJson) -> IvyJson {
    object([
        ("id", id),
        ("status", "completed".into()),
        ("itemsView", "notLoaded".into()),
        ("items", IvyJson::Array(vec![])),
    ])
}
fn page(data: Vec<IvyJson>, next: Option<&str>) -> IvyJson {
    object([
        ("data", IvyJson::Array(data)),
        (
            "nextCursor",
            next.map(IvyJson::from).unwrap_or(IvyJson::Null),
        ),
    ])
}
fn item(id: &str, text: &str) -> IvyJson {
    object([
        ("id", id.into()),
        ("type", "agentMessage".into()),
        ("phase", "final_answer".into()),
        ("text", text.into()),
    ])
}
fn wrapped(turn: &str, item: IvyJson) -> IvyJson {
    object([("turnId", turn.into()), ("item", item)])
}
async fn timed<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(30), future)
        .await
        .unwrap()
}
fn query(request: &IvyJson, method: &str, cursor: Option<&str>) {
    assert_eq!(request.get("method"), Some(&method.into()));
    let params = request.required("params").unwrap();
    assert_eq!(params.get("threadId"), Some(&THREAD.into()));
    assert_eq!(params.get("limit"), Some(&16.0.into()));
    assert_eq!(
        params.get("cursor"),
        Some(&cursor.map(IvyJson::from).unwrap_or(IvyJson::Null))
    );
    if method == "thread/turns/list" {
        assert_eq!(params.get("itemsView"), Some(&"notLoaded".into()));
        assert_eq!(params.get("sortDirection"), Some(&"desc".into()));
    } else {
        assert_eq!(params.get("turnId"), Some(&TURN.into()));
        assert_eq!(params.get("sortDirection"), Some(&"asc".into()));
    }
}
async fn connected() -> (VoiceControl, VoiceRpc, Peer) {
    let (control, _, mut peers, _) = fixture(
        vec![Plan::default()],
        object([("cwd", "C:\\work".into())]),
        hooks(),
    );
    timed(control.open().wait()).await.unwrap();
    let peer = timed(peers.recv()).await.unwrap();
    let rpc = control.current().unwrap();
    (control, rpc, peer)
}
async fn close(control: VoiceControl) {
    timed(control.close(Box::pin(async { Ok(()) })).wait())
        .await
        .unwrap();
}

#[tokio::test]
async fn known_turn_reads_metadata_pages_and_only_its_ordered_items() {
    let (control, rpc, mut peer) = connected().await;
    let work = tokio::spawn(async move {
        rpc.preparation_turn(&THREAD.into(), Some(&TURN.into()), &OPERATION.into())
            .await
    });
    for n in 0..3 {
        let request = peer.request().await;
        let cursor = (n > 0).then(|| format!("turn-page-{n}"));
        query(&request, "thread/turns/list", cursor.as_deref());
        let mut turns: Vec<_> = (0..16)
            .map(|i| turn(format!("unrelated-{n}-{i}").into()))
            .collect();
        if n == 2 {
            turns[5] = turn(TURN.into());
        }
        peer.reply(&request, page(turns, Some(&format!("turn-page-{}", n + 1))))
            .await;
    }
    for n in 0..2 {
        let request = peer.request().await;
        query(
            &request,
            "thread/items/list",
            (n == 1).then_some("item-page-1"),
        );
        let entries = if n == 0 {
            (0..16)
                .map(|i| wrapped(TURN, item(&format!("original-{i}"), &format!("text-{i}"))))
                .collect()
        } else {
            vec![wrapped(TURN, item("original-16", "READY"))]
        };
        peer.reply(&request, page(entries, (n == 0).then_some("item-page-1")))
            .await;
    }
    let result = timed(work).await.unwrap().unwrap().unwrap();
    let items = result.required("items").unwrap().as_array().unwrap();
    assert_eq!(items.len(), 17);
    for (i, entry) in items.iter().enumerate() {
        assert_eq!(entry.get("id"), Some(&format!("original-{i}").into()));
    }
    assert_eq!(items.last().unwrap().get("text"), Some(&"READY".into()));
    close(control).await;
}

#[tokio::test]
async fn missing_turn_reference_finds_the_original_client_operation_without_a_start() {
    let (control, rpc, mut peer) = connected().await;
    let work = tokio::spawn(async move {
        rpc.preparation_turn(&THREAD.into(), None, &OPERATION.into())
            .await
    });
    let request = peer.request().await;
    query(&request, "thread/turns/list", None);
    peer.reply(
        &request,
        page(vec![turn("other-turn".into())], Some("older")),
    )
    .await;
    let request = peer.request().await;
    assert_eq!(request.get("method"), Some(&"thread/items/list".into()));
    assert_eq!(
        request.required("params").unwrap().get("turnId"),
        Some(&"other-turn".into())
    );
    peer.reply(
        &request,
        page(
            vec![wrapped(
                "other-turn",
                object([
                    ("id", "other-input".into()),
                    ("type", "userMessage".into()),
                    ("clientId", "another-operation".into()),
                ]),
            )],
            None,
        ),
    )
    .await;
    let request = peer.request().await;
    query(&request, "thread/turns/list", Some("older"));
    peer.reply(&request, page(vec![turn(TURN.into())], None))
        .await;
    let original = object([
        ("id", "original-input".into()),
        ("type", "userMessage".into()),
        ("clientId", OPERATION.into()),
    ]);
    for _ in 0..2 {
        let request = peer.request().await;
        query(&request, "thread/items/list", None);
        peer.reply(
            &request,
            page(
                vec![
                    wrapped(TURN, original.clone()),
                    wrapped(TURN, item("final", "READY")),
                ],
                None,
            ),
        )
        .await;
    }
    let result = timed(work).await.unwrap().unwrap().unwrap();
    assert_eq!(result.get("id"), Some(&TURN.into()));
    assert_eq!(
        result.required("items").unwrap().as_array().unwrap()[0],
        original
    );
    close(control).await;
}

#[tokio::test]
async fn cursor_loops_mixed_turns_and_duplicate_items_remain_unresolved() {
    let (control, rpc, mut peer) = connected().await;
    for mode in ["cursor", "wrong-turn", "duplicate", "bytes"] {
        let reader = rpc.clone();
        let work = tokio::spawn(async move {
            reader
                .preparation_turn(&THREAD.into(), Some(&TURN.into()), &OPERATION.into())
                .await
        });
        let request = peer.request().await;
        query(&request, "thread/turns/list", None);
        if mode == "cursor" {
            peer.reply(&request, page(vec![turn("one".into())], Some("same")))
                .await;
            let request = peer.request().await;
            query(&request, "thread/turns/list", Some("same"));
            peer.reply(&request, page(vec![turn("two".into())], Some("same")))
                .await;
        } else {
            peer.reply(&request, page(vec![turn(TURN.into())], None))
                .await;
            let request = peer.request().await;
            query(&request, "thread/items/list", None);
            let entries = match mode {
                "wrong-turn" => vec![wrapped("another-turn", item("wrong", "READY"))],
                "duplicate" => vec![
                    wrapped(TURN, item("same", "one")),
                    wrapped(TURN, item("same", "two")),
                ],
                "bytes" => vec![wrapped(TURN, item("large", &"x".repeat(PAGE_BYTES)))],
                _ => unreachable!(),
            };
            peer.reply(&request, page(entries, None)).await;
        }
        let error = timed(work).await.unwrap().unwrap_err();
        assert_eq!(
            error.code,
            match mode {
                "wrong-turn" => "native_turn_mismatch",
                "bytes" => "phone_voice_history_limit",
                _ => "phone_voice_history_invalid",
            }
        );
    }
    close(control).await;
}
