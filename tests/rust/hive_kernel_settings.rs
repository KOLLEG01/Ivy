use super::*;

fn ok(steps: &mut Vec<Value>, mut step: Value) -> usize {
    step["requireSuccess"] = true.into();
    push(steps, step)
}
fn settings(revision: u64, quiet: Value, nonce: &str) -> Value {
    request(
        "settings.save",
        json!({"mutationId":format!("epoch:100000:{nonce}"),"expectedRevision":revision,"settings":{"schemaVersion":1,"quietHours":quiet}}),
    )
}
fn openai(revision: u64, app: Value, nonce: &str) -> Value {
    request(
        "openai.saveConfiguration",
        json!({"mutationId":format!("epoch:100000:{nonce}"),"expectedRevision":revision,"configuration":{"schemaVersion":1,"appId":app}}),
    )
}
fn value(result: &Value, index: usize) -> Value {
    result["rows"][index]["value"]["value"].clone()
}

#[test]
fn settings_and_openai_preserve_cas_receipts_quiet_validation_paths_and_reopen() {
    // Extend the existing settings/OpenAI persistence cases with full durable
    // state comparison, failed-write retry, field ordering and path guards.
    let mut s = setup();
    s[0]["systemSettings"] = true.into();
    s[0]["options"] = json!({"validateMessages":true});
    let initial = ok(&mut s, request("settings.get", json!({})));
    let initial_openai = ok(&mut s, request("openai.getConfiguration", json!({})));
    let quiet = json!({"start":"23:00","end":"07:00","timeZone":"Europe/Berlin"});
    let first = settings(0, quiet.clone(), "quiet-first");
    let saved = ok(&mut s, first.clone());
    let replay = ok(&mut s, first.clone());
    let stale = push(&mut s, settings(0, quiet.clone(), "quiet-stale"));
    let equal = push(
        &mut s,
        settings(
            999,
            json!({"start":"23:00","end":"23:00","timeZone":"invalid/zone"}),
            "quiet-equal",
        ),
    );
    let invalid = push(
        &mut s,
        settings(
            999,
            json!({"start":"23:00","end":"07:00","timeZone":"invalid/zone"}),
            "quiet-zone",
        ),
    );
    let malformed = push(
        &mut s,
        settings(
            1,
            json!({"start":"24:00","end":"07:00","timeZone":"Europe/Berlin"}),
            "quiet-format",
        ),
    );
    let utc = ok(
        &mut s,
        settings(
            1,
            json!({"start":"00:00","end":"23:59","timeZone":"UTC"}),
            "quiet-utc",
        ),
    );
    let cleared = ok(&mut s, settings(2, Value::Null, "quiet-null"));
    let old_replay = ok(&mut s, first.clone());
    let mut changed = first;
    changed["request"]["params"]["settings"]["quietHours"] = Value::Null;
    let conflict = push(&mut s, changed);
    let read = ok(
        &mut s,
        request("objects.read", json!({"path":"/ivy-settings"})),
    );
    let mut forbidden = request(
        "objects.write",
        json!({"mutationId":"epoch:100000:quiet-forbidden","objectId":"bound","expectedRevision":3,"contractVersion":"1.0.0","references":{},"content":{"encoding":"json","value":{"schemaVersion":1,"quietHours":null}}}),
    );
    bind(
        &mut forbidden,
        "/request/params/objectId",
        read,
        "/value/value/object/id",
    );
    let forbidden = push(&mut s, forbidden);
    let app = json!(format!("asdk_app_{}", "a".repeat(32)));
    let first_openai = openai(0, app, "openai-first");
    let app_saved = ok(&mut s, first_openai.clone());
    ok(&mut s, first_openai.clone());
    let app_stale = push(&mut s, openai(0, Value::Null, "openai-stale"));
    let app_invalid = push(
        &mut s,
        openai(1, json!("asdk_app_invalid"), "openai-invalid"),
    );
    ok(&mut s, openai(1, Value::Null, "openai-null"));
    let app_replay = ok(&mut s, first_openai);
    s.push(sql(
        "UPDATE objects SET effective_archive=1 WHERE path='/ivy-settings';",
    ));
    let archived_get = push(&mut s, request("settings.get", json!({})));
    let denied = settings(3, Value::Null, "quiet-archived");
    let archived_save = push(&mut s, denied.clone());
    s.push(sql(
        "UPDATE objects SET effective_archive=0 WHERE path='/ivy-settings';",
    ));
    let archived_replay = ok(&mut s, denied);
    ok(&mut s, settings(4, Value::Null, "quiet-recovered"));
    s.push(sql(
        "UPDATE objects SET effective_archive=1 WHERE path='/ivy-openai-configuration';",
    ));
    let app_archived = push(&mut s, request("openai.getConfiguration", json!({})));
    s.push(sql(
        "UPDATE objects SET effective_archive=0 WHERE path='/ivy-openai-configuration';",
    ));
    ok(&mut s, op("reopen"));
    let reopened = ok(&mut s, request("settings.get", json!({})));
    let app_reopened = ok(&mut s, request("openai.getConfiguration", json!({})));

    let actual = compare(&s);
    assert_eq!(
        value(&actual, initial),
        json!({"revision":0,"settings":{"schemaVersion":1,"quietHours":null}})
    );
    assert_eq!(
        value(&actual, initial_openai),
        json!({"revision":0,"configuration":{"schemaVersion":1,"appId":null}})
    );
    assert_eq!(value(&actual, saved), value(&actual, replay));
    assert_eq!(value(&actual, saved), value(&actual, old_replay));
    assert_eq!(value(&actual, utc)["revision"], 2);
    assert_eq!(value(&actual, cleared)["revision"], 3);
    assert_eq!(value(&actual, app_saved), value(&actual, app_replay));
    for i in [stale, app_stale] {
        error(&actual, i, "revision_conflict");
    }
    for i in [equal, invalid, malformed, app_invalid] {
        error(&actual, i, "invalid_arguments");
    }
    error(&actual, conflict, "mutation_conflict");
    error(&actual, forbidden, "forbidden");
    for i in [archived_get, archived_save, app_archived] {
        error(&actual, i, "target_conflict");
    }
    assert_eq!(
        actual["rows"][archived_save]["state"]["mutations"],
        actual["rows"][archived_save - 1]["state"]["mutations"]
    );
    assert_eq!(value(&actual, archived_replay)["revision"], 4);
    assert_eq!(
        value(&actual, reopened),
        json!({"revision":5,"settings":{"schemaVersion":1,"quietHours":null}})
    );
    assert_eq!(
        value(&actual, app_reopened),
        json!({"revision":2,"configuration":{"schemaVersion":1,"appId":null}})
    );
}
