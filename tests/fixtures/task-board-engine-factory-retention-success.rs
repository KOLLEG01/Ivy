use super::*;

#[track_caller]
fn same_json(left: &Json, right: &Json) {
    assert_eq!(hash_json(left).unwrap(), hash_json(right).unwrap());
}

pub(super) async fn pending(
    fixture: &Fixture,
    peer: &Peer,
    control: &ServiceControl,
    connection: &ServiceConnection,
) {
    let source = original(&Json::object([
        ("configurationControl", "request".into()),
        (
            "callerPrincipalId",
            Json::String(context(1).caller_principal_id),
        ),
        (
            "owner",
            Json::object([
                ("serviceNodeId", "fixture-task-board".into()),
                ("generation", Json::Number(1.0)),
            ]),
        ),
    ]));
    let request = f(&source, "request").clone();
    admission("ActionRequest", &request).unwrap();
    admission("DurableRequest", &request).unwrap();
    rpc(
        connection,
        "test.retentionConfiguration",
        Json::object(Vec::<(&str, Json)>::new()),
    )
    .await
    .unwrap();
    let before_publication = tables(fixture);
    let outcome = bounded(control.handler("update")(
        request.clone(),
        c(1, "retention-control"),
    ))
    .await
    .unwrap();
    let saved = operation(fixture, "retention-control");
    assert_eq!(f(&saved.2, "phase"), &Json::from("succeeded"));
    assert_eq!(f(&saved.2, "error"), &Json::Null);
    same_json(f(&saved.2, "request"), &request);
    assert_eq!(
        f(&saved.2, "requestHash"),
        &Json::from(hash_json(&request).unwrap())
    );
    assert_eq!(
        f(&saved.2, "callerPrincipalId"),
        &Json::String(context(1).caller_principal_id)
    );
    assert_eq!(f(f(&saved.2, "actor"), "generation"), &Json::Number(1.0));
    same_json(f(&saved.2, "actor"), f(&source, "actor"));
    same_json(f(&saved.2, "outcome"), &outcome);
    let writes = f(&saved.2, "writes").as_array().unwrap();
    assert_eq!(writes.len(), 1);
    assert_eq!(
        f(&writes[0], "contractKey"),
        &Json::from("task-board/configuration")
    );
    same_json(f(&writes[0], "outcome"), f(&outcome, "configuration"));
    let configuration = fixture.rows("task-board/configuration");
    assert_eq!(configuration.len(), 1);
    same_json(&configuration[0].2, f(&request, "value"));
    assert_eq!(configuration[0].1, 1);
    let discovery = frames(peer, "retention-configuration-hosts");
    assert_eq!(
        discovery.len(),
        f(&source, "trace").as_array().unwrap().len()
    );
    for (actual, expected) in discovery
        .iter()
        .zip(f(&source, "trace").as_array().unwrap())
    {
        same_json(f(actual, "params"), &expected.as_array().unwrap()[1]);
    }
    assert_eq!(fixture.rows("task-board/operation").len(), 2);
    assert_eq!(count(fixture, "SELECT count(*) FROM active_gates"), 0);
    let before = tables(fixture);
    let original_cached = original(&Json::object([
        ("configurationControl", "cached".into()),
        (
            "operation",
            Json::object([
                (
                    "pin",
                    Json::object([
                        ("objectId", saved.0.clone().into()),
                        ("revision", Json::Number(saved.1 as f64)),
                    ]),
                ),
                ("value", saved.2.clone()),
            ]),
        ),
    ]));
    same_json(f(&original_cached, "outcome"), &outcome);
    assert_eq!(
        f(&original_cached, "trace"),
        &Json::Array(vec![saved.0.clone().into()])
    );
    let replay = bounded(control.handler("update")(
        request.clone(),
        c(1, "retention-control"),
    ))
    .await
    .unwrap();
    same_json(&replay, &outcome);
    assert_eq!(tables(fixture), before);
    assert_eq!(
        frames(peer, "retention-configuration-hosts").len(),
        discovery.len()
    );
    proof(
        "successful-control-before-eviction",
        Json::object([
            ("source", source),
            ("operation", saved.2.clone()),
            ("originalCached", original_cached),
            ("beforePublication", before_publication),
            ("tables", before.clone()),
            ("configuration", configuration[0].2.clone()),
        ]),
    );
    // Generous rows make bytes the only pressure trigger. No artificial age or
    // deletion of the linked question publication is used.
    checkpoint(fixture, peer, connection, "successful-byte-control-evicted").await;
    assert_eq!(fixture.rows("task-board/operation").len(), 1);
    assert_eq!(fixture.rows("task-board/configuration"), configuration);
    assert_eq!(count(fixture, "SELECT count(*) FROM retired_operations"), 1);
    assert!(count(fixture, "SELECT SUM(rows) FROM storage_usage") < 10000);
    assert!(count(fixture, "SELECT SUM(bytes) FROM storage_usage") > 0);
    let after = tables(fixture);
    let expired = bounded(control.handler("update")(
        request,
        c(1, "retention-control"),
    ))
    .await
    .unwrap_err();
    assert_eq!(expired.code, "operation_expired");
    assert_eq!(expired.outcome, Outcome::NotExecuted);
    assert_eq!(tables(fixture), after);
    assert_eq!(
        frames(peer, "retention-configuration-hosts").len(),
        discovery.len()
    );
    let original_expired = original(&Json::object([
        ("tables", after.clone()),
        (
            "limits",
            Json::object([
                ("count", Json::Number(10000.0)),
                ("bytes", Json::Number(0.0)),
            ]),
        ),
        ("nowMs", Json::Number(fixture.clock.now_millis())),
        ("remote", Json::Object(vec![])),
        (
            "lookups",
            Json::Array(vec![Json::Array(vec![
                Json::String(context(1).caller_principal_id),
                "retention-control".into(),
            ])]),
        ),
    ]));
    same_json(f(&original_expired, "tables"), &after);
    let error = f(
        &f(&original_expired, "lookups").as_array().unwrap()[0],
        "error",
    );
    assert_eq!(f(error, "code"), &Json::from(expired.code.clone()));
    assert_eq!(f(error, "message"), &Json::from(expired.message.clone()));
    assert_eq!(f(error, "outcome"), &Json::from(expired.outcome.as_str()));
    proof(
        "expired-successful-control-replay",
        Json::object([
            ("operation", saved.2),
            ("originalExpired", original_expired),
            ("configuration", configuration[0].2.clone()),
            ("tables", after),
        ]),
    );
}
