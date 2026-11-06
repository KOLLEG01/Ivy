use super::*;
use ivy_agent_manager::claude::protocol::{Adapter, Effect};

fn response(effects: &[Effect], id: &str) -> J {
    effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Send { message, .. } if f(message, "id") == &J::from(id) => {
                Some(f(message, "result").clone())
            }
            _ => None,
        })
        .expect("Original native response missing")
}
fn request(adapter: &mut Adapter, id: &str, method: &str, params: J) -> J {
    response(
        &adapter
            .receive(
                &"peer".into(),
                &J::object([
                    ("id", id.into()),
                    ("method", method.into()),
                    ("params", params),
                ]),
            )
            .unwrap(),
        id,
    )
}
fn start(root: &Path) -> (Adapter, JsString) {
    let mut adapter = super::driver_cases::adapter(root);
    let thread = request(
        &mut adapter,
        "thread",
        "thread/start",
        v(r#"{"approvalPolicy":"on-request","sandbox":"workspace-write"}"#),
    );
    let turn = request(
        &mut adapter,
        "turn",
        "turn/start",
        J::object([
            ("threadId", f(f(&thread, "thread"), "id").clone()),
            (
                "input",
                v(r#"[{"type":"text","text":"original callback"}]"#),
            ),
        ]),
    );
    let turn = s(f(f(&turn, "turn"), "id")).clone();
    adapter.begin_turn(&turn).unwrap();
    (adapter, turn)
}
fn permission(index: usize) -> J {
    J::object([
        ("toolUseId", format!("original-tool-{index}").into()),
        ("toolName", "Bash".into()),
        ("input", v(r#"{"command":"original command\ud800"}"#)),
    ])
}
fn original(index: usize, bytes: usize) -> J {
    let mut text = Vec::with_capacity(bytes / 2);
    text.extend([65, 0xd800]);
    J::object([
        ("callbackId", (index as f64).into()),
        ("original", J::String(JsString(text))),
    ])
}
fn approval(effects: &[Effect]) -> J {
    effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Send { message, .. }
                if f(message, "method") == &J::from("item/commandExecution/requestApproval") =>
            {
                Some(f(message, "id").clone())
            }
            _ => None,
        })
        .expect("Original approval missing")
}

#[test]
fn original_reverse_requests_have_a_finite_count_and_recover_only_after_resolution() {
    let root = Root::new();
    let (mut adapter, turn) = start(&root.0);
    let mut ids = vec![];
    for index in 0..32 {
        ids.push(approval(
            &adapter
                .permission_request(&turn, &permission(index), original(index, 4))
                .unwrap(),
        ));
    }
    let error = adapter
        .permission_request(&turn, &permission(32), original(32, 4))
        .expect_err("A 33rd unresolved original reverse request was admitted");
    assert_eq!(error.name, "native_capacity");
    let first = ids.remove(0);
    let answer = J::object([
        ("id", first.clone()),
        ("result", v(r#"{"decision":"decline"}"#)),
    ]);
    let effects = adapter.receive(&"peer".into(), &answer).unwrap();
    assert_eq!(effects.iter().filter(|effect| matches!(effect, Effect::Permission { callback, .. } if f(callback, "callbackId") == &J::from(0.0))).count(), 1);
    assert!(adapter.receive(&"peer".into(), &answer).unwrap().is_empty());
    ids.push(approval(
        &adapter
            .permission_request(&turn, &permission(32), original(32, 4))
            .unwrap(),
    ));
    for (offset, id) in ids.into_iter().enumerate() {
        let effects = adapter
            .receive(
                &"peer".into(),
                &J::object([("id", id), ("result", v(r#"{"decision":"decline"}"#))]),
            )
            .unwrap();
        assert_eq!(effects.iter().filter(|effect| matches!(effect, Effect::Permission { callback, .. } if f(callback, "callbackId") == &J::from((offset + 1) as f64))).count(), 1);
    }
    adapter.stop().unwrap();
}

#[test]
fn reverse_heap_budget_keeps_the_original_opaque_callback_capacity_until_resolution() {
    let root = Root::new();
    let (mut adapter, turn) = start(&root.0);
    let mut ids = vec![];
    for index in 0..2 {
        ids.push(approval(
            &adapter
                .permission_request(&turn, &permission(index), original(index, 24 * 1024 * 1024))
                .unwrap(),
        ));
    }
    let error = adapter
        .permission_request(&turn, &permission(2), original(2, 24 * 1024 * 1024))
        .expect_err(
            "Three original 24MiB retained callbacks fit an allegedly bounded reverse owner",
        );
    assert_eq!(error.name, "native_capacity");
    let effects = adapter
        .receive(
            &"peer".into(),
            &J::object([
                ("id", ids.remove(0)),
                ("result", v(r#"{"decision":"decline"}"#)),
            ]),
        )
        .unwrap();
    let callback = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Permission { callback, .. } => Some(callback),
            _ => None,
        })
        .unwrap();
    assert_eq!(f(callback, "callbackId"), &J::from(0.0));
    let original = s(f(callback, "original"));
    assert_eq!(original.0, [65, 0xd800]);
    assert_eq!(original.0.capacity(), 12 * 1024 * 1024);
    // The returned effect is a distinct publication lifetime, outside this pending-only quota.
    drop(effects);
    ids.push(approval(
        &adapter
            .permission_request(&turn, &permission(2), self::original(2, 24 * 1024 * 1024))
            .unwrap(),
    ));
    for (offset, id) in ids.into_iter().enumerate() {
        let effects = adapter
            .receive(
                &"peer".into(),
                &J::object([("id", id), ("result", v(r#"{"decision":"decline"}"#))]),
            )
            .unwrap();
        assert!(effects.iter().any(|effect| matches!(effect, Effect::Permission { callback, .. } if f(callback, "callbackId") == &J::from((offset + 1) as f64))));
    }
    adapter.stop().unwrap();
}

#[test]
fn original_cancellation_releases_only_its_request_and_keeps_other_questions_live() {
    let root = Root::new();
    let (mut adapter, turn) = start(&root.0);
    let permission_id = approval(
        &adapter
            .permission_request(&turn, &permission(0), original(0, 4))
            .unwrap(),
    );
    let event = v(
        r#"{"toolUseId":"original-ask","questions":[{"id":"q0","header":"Pick","question":"Which?\ud800","isOther":false,"isSecret":false,"options":[{"label":"A","description":"first"}]}]}"#,
    );
    let effects = adapter
        .user_input_request(&turn, &event, original(1, 4))
        .unwrap();
    let question_id = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Send { message, .. }
                if f(message, "method") == &J::from("item/tool/requestUserInput") =>
            {
                Some(f(message, "id").clone())
            }
            _ => None,
        })
        .unwrap();
    assert!(
        adapter
            .cancel_callbacks(&"different-original-turn".into(), Some(0))
            .unwrap()
            .is_empty()
    );
    assert!(
        adapter
            .cancel_callbacks(&turn, Some(99))
            .unwrap()
            .is_empty()
    );
    let effects = adapter.cancel_callbacks(&turn, Some(0)).unwrap();
    assert_eq!(effects.iter().filter(|effect| matches!(effect, Effect::Send { message, .. }
        if f(message, "method") == &J::from("serverRequest/resolved") && f(f(message, "params"), "requestId") == &permission_id)).count(), 1);
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Permission { .. }))
    );
    assert!(effects.iter().any(|effect| matches!(effect, Effect::Send { message, .. }
        if f(f(message, "params"), "status") == &v(r#"{"type":"active","activeFlags":["waitingOnUserInput"]}"#))));
    assert!(adapter.cancel_callbacks(&turn, Some(0)).unwrap().is_empty());
    assert!(
        adapter
            .receive(
                &"peer".into(),
                &J::object([
                    ("id", permission_id),
                    ("result", v(r#"{"decision":"acceptForSession"}"#)),
                ])
            )
            .unwrap()
            .is_empty()
    );
    let effects = adapter.receive(&"peer".into(), &J::object([
        ("id", question_id), ("result", v(r#"{"answers":{"q0":{"answers":["Other"],"notes":"original answer\ud800"}}}"#)),
    ])).unwrap();
    let answers = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Permission { callback, result }
                if f(callback, "callbackId") == &J::from(1.0) =>
            {
                Some(result)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(
        f(f(f(answers, "answers"), "q0"), "notes"),
        &v(r#""original answer\ud800""#)
    );
    assert!(effects.iter().any(|effect| matches!(effect, Effect::Permission { callback, .. }
        if f(callback, "callbackId") == &J::from(1.0) && s(f(callback, "original")).0 == [65, 0xd800])));
    for cycle in 0..2 {
        let mut ids = vec![];
        for index in 0..32 {
            ids.push(approval(
                &adapter
                    .permission_request(
                        &turn,
                        &permission(2 + cycle * 32 + index),
                        original(2 + cycle * 32 + index, 4),
                    )
                    .unwrap(),
            ));
        }
        let effects = adapter.cancel_callbacks(&turn, None).unwrap();
        let resolved = effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Send { message, .. }
                    if f(message, "method") == &J::from("serverRequest/resolved") =>
                {
                    Some(f(f(message, "params"), "requestId").clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(resolved, ids);
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::Permission { .. }))
        );
    }
    adapter.stop().unwrap();
}
