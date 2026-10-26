use super::{
    AuthContext, AuthenticatedContext, IvyError, IvyJson, Root, Store, native, reference_data,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ivy_contracts::{IvyResult, hash::digest};
use ivy_hive::{store::Credential, worker_identity as identity};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) fn bind(step: &mut Value, rows: &[Value]) {
    for binding in step["identityBindings"].as_array().unwrap().clone() {
        let selected = rows[binding["from"].as_u64().unwrap() as usize]
            .pointer(binding["select"].as_str().unwrap())
            .unwrap();
        let selected = if binding["digest"] == true {
            json!(digest(selected.as_str().unwrap()))
        } else {
            selected.clone()
        };
        *step.pointer_mut(binding["at"].as_str().unwrap()).unwrap() = selected;
    }
}
pub(super) fn native_identity(store: &mut Store, step: &Value) -> IvyResult<IvyJson> {
    let params = IvyJson::from_serde(&step["params"]).unwrap();
    let text = |key| params.get(key).and_then(IvyJson::as_js_string).unwrap();
    let references = |name| {
        params
            .get(name)
            .and_then(IvyJson::as_array)
            .unwrap()
            .iter()
            .map(|v| v.as_js_string().unwrap().clone())
            .collect::<Vec<_>>()
    };
    let now = step["now"].as_u64().unwrap();
    match step["method"].as_str().unwrap() {
        "oauth.initialize" => identity::oauth_secrets(store),
        "credentialReference" => {
            identity::credential_reference(store, text("digest")).map(IvyJson::String)
        }
        "oauth.family.create" => identity::create_oauth_family(
            store,
            params.get("state").unwrap(),
            &references("activeCredentialReferences"),
            now,
        ),
        "oauth.family.rotate" => {
            identity::rotate_oauth_family(store, params.get("rotation").unwrap(), now)
        }
        "collect" => {
            let active = params.get("active").map(|_| references("active"));
            store.transaction(|s| {
                identity::collect_oauth_families(s, now, active.as_deref())
                    .map(|n| IvyJson::Number(n as f64))
            })
        }
        "credentials.configure" => {
            let credentials = params
                .get("credentials")
                .and_then(IvyJson::as_array)
                .unwrap()
                .iter()
                .map(|v| Credential {
                    principal_id: v
                        .get("principalId")
                        .and_then(IvyJson::as_js_string)
                        .unwrap()
                        .clone(),
                    digest: v
                        .get("digest")
                        .and_then(IvyJson::as_js_string)
                        .unwrap()
                        .clone(),
                })
                .collect::<Vec<_>>();
            identity::configure_credentials(store, &credentials, now)
        }
        "authenticate" => {
            let c = params.get("context").unwrap();
            let p = store.authenticate(&AuthContext {
                credential_digest: c
                    .get("credentialDigest")
                    .and_then(IvyJson::as_js_string)
                    .unwrap()
                    .clone(),
                session_digest: c
                    .get("sessionDigest")
                    .and_then(IvyJson::as_js_string)
                    .cloned(),
            })?;
            Ok(IvyJson::object([
                ("principalId", IvyJson::String(p.principal_id)),
                ("credentialDigest", IvyJson::String(p.credential_digest)),
            ]))
        }
        "session.create" => identity::create_session(
            store,
            text("credentialDigest"),
            &step["nowText"].as_str().unwrap().into(),
        ),
        "session.resolve" => identity::resolve_session(store, text("sessionDigest")),
        "session.end" => {
            let c = params.get("context").unwrap();
            identity::end_session(
                store,
                &AuthenticatedContext {
                    principal_id: c
                        .get("principalId")
                        .and_then(IvyJson::as_js_string)
                        .unwrap()
                        .clone(),
                    credential_digest: c
                        .get("credentialDigest")
                        .and_then(IvyJson::as_js_string)
                        .unwrap()
                        .clone(),
                },
                c.get("sessionDigest").and_then(IvyJson::as_js_string),
            )
        }
        "execute" => {
            identity::check_access_expiry(params.get("context").unwrap(), now)?;
            Ok(IvyJson::object([(
                "forwarded",
                params.get("request").unwrap().clone(),
            )]))
        }
        _ => Err(IvyError::internal()),
    }
}
fn sql(text: impl Into<String>) -> Value {
    json!({"op":"sql","sql":text.into()})
}
fn credential(unit: &str) -> String {
    format!("sha256:{}", unit.repeat(64))
}
fn action(method: &str, params: Value) -> Value {
    json!({"op":"identity","method":method,"params":params,"now":100000,"nowText":"1970-01-01T00:01:40.000Z"})
}
fn setup() -> Vec<Value> {
    vec![
        sql(format!(
            "UPDATE metadata SET value='{}' WHERE key='cursor_secret';",
            "0123456789abcdef".repeat(4)
        )),
        json!({"op":"configure","credentials":[{"principalId":"alpha","digest":credential("a")},{"principalId":"beta","digest":credential("b")}]}),
    ]
}
fn reference(steps: &[Value]) -> Value {
    static CASES: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    let cases = CASES.get_or_init(|| {
        serde_json::from_slice(&reference_data::read(include_bytes!(
            "../fixtures/hive-worker-identity-reference.json.gz"
        )))
        .unwrap()
    });
    let key = digest(serde_json::to_vec(steps).unwrap());
    cases["traces"]
        .get(&key)
        .unwrap_or_else(|| panic!("Missing original Hive identity trace: {key}"))
        .clone()
}
fn substitute(value: &Value, aliases: &BTreeMap<String, String>) -> Value {
    match value {
        Value::String(text) => Value::String(aliases.get(text).unwrap_or(text).clone()),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| substitute(item, aliases)).collect())
        }
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), substitute(value, aliases)))
                .collect(),
        ),
        value => value.clone(),
    }
}
fn compare(steps: &[Value]) -> Value {
    let root = Root::new();
    let actual = native(&root.0.join("native.sqlite"), steps);
    let expected = reference(steps);
    assert_eq!(actual["rows"].as_array().unwrap().len(), steps.len());
    assert_eq!(expected["rows"].as_array().unwrap().len(), steps.len());
    let mut aliases = BTreeMap::new();
    for (index, step) in steps.iter().enumerate() {
        if step["op"] == "identity" && step["method"] == "session.create" {
            if let Some(token) = actual["rows"][index]["value"]["sessionToken"].as_str() {
                assert_eq!(token.len(), 43);
                let bytes = URL_SAFE_NO_PAD.decode(token).unwrap();
                assert_eq!(bytes.len(), 32);
                let mut fixed = [0u8; 32];
                fixed[..8].copy_from_slice(&(index as u64).to_le_bytes());
                let fixed = URL_SAFE_NO_PAD.encode(fixed);
                assert_eq!(expected["rows"][index]["value"]["sessionToken"], fixed);
                assert!(aliases.insert(token.to_owned(), fixed.clone()).is_none());
                assert!(aliases.insert(digest(token), digest(&fixed)).is_none());
            }
        }
    }
    for (index, (left, right)) in actual["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected["rows"].as_array().unwrap())
        .enumerate()
    {
        let mut left = substitute(left, &aliases);
        left["state"]["sessions"]
            .as_array_mut()
            .unwrap()
            .sort_by(|a, b| a["digest"].as_str().cmp(&b["digest"].as_str()));
        assert_eq!(
            &left, right,
            "Worker identity trace {index} ({})",
            steps[index]["method"]
        );
        if matches!(steps[index]["op"].as_str(), Some("sql" | "configure")) {
            assert!(right["error"].is_null(), "Fixture setup failed at {index}");
        }
    }
    assert_eq!(actual["definitions"], expected["definitions"]);
    assert_eq!(actual["metadata"], expected["metadata"]);
    actual
}
fn bind_value(step: &mut Value, at: &str, from: usize, select: &str, hash: bool) {
    if !step["identityBindings"].is_array() {
        step["identityBindings"] = json!([]);
    }
    step["identityBindings"]
        .as_array_mut()
        .unwrap()
        .push(json!({"at":at,"from":from,"select":select,"digest":hash}));
}
fn resolve(from: usize) -> Value {
    let mut step = action("session.resolve", json!({"sessionDigest":"bound"}));
    bind_value(
        &mut step,
        "/params/sessionDigest",
        from,
        "/value/sessionToken",
        true,
    );
    step
}
fn end(from: usize) -> Value {
    let mut step = action(
        "session.end",
        json!({"context":{"principalId":"alpha","credentialDigest":credential("a"),"sessionDigest":"bound"}}),
    );
    bind_value(
        &mut step,
        "/params/context/sessionDigest",
        from,
        "/value/sessionToken",
        true,
    );
    step
}
fn family(id: &str) -> Value {
    json!({"familyDigest":id,"credentialReference":"ref-alpha","clientId":"client","resource":"https://fixture.invalid/mcp","scope":"hive","generation":1,"currentTokenDigest":"token-1","expiresAt":500000,"revoked":false,"extra":{"retained":true}})
}
fn create(id: &str) -> Value {
    action(
        "oauth.family.create",
        json!({"state":family(id),"activeCredentialReferences":["ref-alpha"]}),
    )
}
fn rotation(id: &str) -> Value {
    action(
        "oauth.family.rotate",
        json!({"rotation":{"familyDigest":id,"credentialReference":"ref-alpha","clientId":"client","resource":"https://fixture.invalid/mcp","scope":"hive","presentedGeneration":1,"presentedTokenDigest":"token-1","nextGeneration":2,"nextTokenDigest":"token-2","nextExpiresAt":600000}}),
    )
}
fn value(rows: &Value, index: usize) -> &Value {
    &rows["rows"][index]["value"]
}
fn state(rows: &Value, index: usize) -> &Value {
    &rows["rows"][index]["state"]
}
fn success(rows: &Value, index: usize) {
    assert!(
        rows["rows"][index]["error"].is_null(),
        "Identity {index}: {}",
        rows["rows"][index]["error"]
    );
}
fn code(rows: &Value, index: usize, code: &str) {
    assert_eq!(rows["rows"][index]["error"]["data"]["code"], code);
}
fn no_effect(rows: &Value, index: usize, expected: &str) {
    code(rows, index, expected);
    assert_eq!(state(rows, index), state(rows, index - 1));
}
fn retained(rows: &Value, index: usize, key: &str) -> Value {
    let raw = state(rows, index)["metadata"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == key)
        .unwrap()["value"]
        .as_str()
        .unwrap();
    serde_json::from_str(raw).unwrap()
}

#[test]
fn worker_keys_and_access_expiry_preserve_v2_labels_root_identity_and_exact_time_boundaries() {
    let mut steps = setup();
    let first = steps.len();
    steps.push(action("oauth.initialize", json!({})));
    let reference = steps.len();
    steps.push(action(
        "credentialReference",
        json!({"digest":credential("a")}),
    ));
    steps.push(json!({"op":"reopen"}));
    let restart = steps.len();
    steps.push(action("oauth.initialize", json!({})));
    let mut checks = vec![];
    for expires in [
        None,
        Some(json!(100001)),
        Some(json!(100000)),
        Some(Value::Null),
        Some(json!("100001")),
        Some(json!({})),
    ] {
        let mut context = json!({});
        if let Some(expires) = expires {
            context["expiresAt"] = expires;
        }
        checks.push(steps.len());
        steps.push(action(
            "execute",
            json!({"context":context,"request":{"fixture":"only expiry admission"}}),
        ));
    }
    let mut coercions = vec![];
    for (expires, admitted) in [
        (json!(" \u{feff}100001\u{feff} "), true),
        (json!("0x20000"), true),
        (json!("0b100000000000000000"), true),
        (json!("0o400000"), true),
        (json!("Infinity"), true),
        (json!("inf"), false),
        (json!("-Infinity"), false),
        (json!([100001]), true),
        (json!([["100001"]]), true),
        (json!([true]), false),
        (json!([100001, 1]), false),
        (json!(true), false),
    ] {
        coercions.push((steps.len(), admitted));
        steps.push(action(
            "execute",
            json!({"context":{"expiresAt":expires},"request":{"fixture":"retained expiry coercion"}}),
        ));
    }
    steps.push(sql(
        "UPDATE metadata SET value=upper(value) WHERE key='cursor_secret';",
    ));
    let invalid = steps.len();
    steps.push(action("oauth.initialize", json!({})));
    let rows = compare(&steps);
    for i in [first, reference, restart, checks[0], checks[1], checks[4]] {
        success(&rows, i);
    }
    assert_eq!(value(&rows, first), value(&rows, restart));
    assert_eq!(
        value(&rows, first)["signingKey"].as_str().unwrap().len(),
        64
    );
    assert!(
        value(&rows, reference)
            .as_str()
            .unwrap()
            .starts_with("ivyr2.")
    );
    for i in [checks[2], checks[3], checks[5]] {
        no_effect(&rows, i, "unauthenticated");
    }
    for (index, admitted) in coercions {
        if admitted {
            success(&rows, index);
        } else {
            no_effect(&rows, index, "unauthenticated");
        }
    }
    no_effect(&rows, invalid, "storage_invalid");
}

#[test]
fn browser_sessions_use_real_entropy_hashed_storage_restart_logout_and_credential_revocation() {
    let mut steps = setup();
    let created = steps.len();
    steps.push(action(
        "session.create",
        json!({"credentialDigest":credential("a")}),
    ));
    let resolved = steps.len();
    steps.push(resolve(created));
    steps.push(json!({"op":"reopen"}));
    steps.push(resolve(created));
    let missing_session = steps.len();
    steps.push(action(
        "session.end",
        json!({"context":{"principalId":"alpha","credentialDigest":credential("a")}}),
    ));
    steps.push(sql("CREATE TRIGGER reject_session BEFORE DELETE ON browser_sessions BEGIN SELECT RAISE(ABORT,'fixture_session'); END;"));
    let failed = steps.len();
    steps.push(end(created));
    steps.push(sql("DROP TRIGGER reject_session;"));
    let ended = steps.len();
    steps.push(end(created));
    steps.push(end(created));
    let inactive = steps.len();
    steps.push(resolve(created));
    let next = steps.len();
    steps.push(action(
        "session.create",
        json!({"credentialDigest":credential("a")}),
    ));
    steps.push(action("credentials.configure",json!({"credentials":[{"principalId":"alpha","digest":credential("c")},{"principalId":"beta","digest":credential("b")}]})));
    let revoked = steps.len();
    steps.push(resolve(next));
    let denied = steps.len();
    steps.push(action(
        "session.create",
        json!({"credentialDigest":credential("a")}),
    ));
    let rows = compare(&steps);
    for i in [created, resolved, resolved + 2, ended, ended + 1, next] {
        success(&rows, i);
    }
    no_effect(&rows, missing_session, "unauthenticated");
    no_effect(&rows, failed, "internal_error");
    no_effect(&rows, inactive, "unauthenticated");
    no_effect(&rows, revoked, "unauthenticated");
    no_effect(&rows, denied, "unauthenticated");
    assert_eq!(
        value(&rows, resolved)["sessionDigest"],
        digest(value(&rows, created)["sessionToken"].as_str().unwrap())
    );
    assert_eq!(value(&rows, resolved)["principalId"], "alpha");
    assert!(
        !state(&rows, created)["sessions"]
            .to_string()
            .contains(value(&rows, created)["sessionToken"].as_str().unwrap())
    );
}

#[test]
fn refresh_families_rotate_once_revoke_reuse_and_foreign_bindings_and_preserve_failed_successors() {
    for (method, parameters) in [
        (
            "oauth.family.create",
            json!({"state":null,"activeCredentialReferences":["ref-alpha"]}),
        ),
        ("oauth.family.rotate", json!({"rotation":null})),
    ] {
        let mut steps = setup();
        let failed = steps.len();
        steps.push(action(method, parameters));
        let rows = compare(&steps);
        no_effect(&rows, failed, "internal_error");
    }
    for (field, invalid) in [
        ("credentialReference", json!("foreign")),
        ("generation", json!(0)),
        ("revoked", json!(true)),
        ("expiresAt", json!(100000)),
    ] {
        let mut steps = setup();
        let mut request = create("invalid-binding");
        request["params"]["state"][field] = invalid;
        let failed = steps.len();
        steps.push(request);
        let rows = compare(&steps);
        no_effect(&rows, failed, "unauthenticated");
    }
    let mut steps = setup();
    let created = steps.len();
    steps.push(create("first"));
    let duplicate = steps.len();
    steps.push(create("first"));
    let invalid = steps.len();
    let mut bad = rotation("first");
    bad["params"]["rotation"]["nextGeneration"] = 3.into();
    steps.push(bad);
    let rotated = steps.len();
    steps.push(rotation("first"));
    let reused = steps.len();
    steps.push(rotation("first"));
    steps.push(json!({"op":"reopen"}));
    let restart = steps.len();
    let mut next = rotation("first");
    next["params"]["rotation"]["presentedGeneration"] = 2.into();
    next["params"]["rotation"]["presentedTokenDigest"] = "token-2".into();
    next["params"]["rotation"]["nextGeneration"] = 3.into();
    steps.push(next);
    let mut bindings = vec![];
    for (key, replacement) in [
        ("credentialReference", "foreign"),
        ("clientId", "foreign"),
        ("resource", "https://fixture.invalid/mcp-dev"),
        ("scope", "foreign"),
        ("presentedTokenDigest", "wrong"),
    ] {
        steps.push(create(key));
        let mut wrong = rotation(key);
        wrong["params"]["rotation"][key] = replacement.into();
        bindings.push((steps.len(), key));
        steps.push(wrong);
    }
    let missing = steps.len();
    steps.push(rotation("missing"));
    let rows = compare(&steps);
    success(&rows, created);
    no_effect(&rows, duplicate, "target_conflict");
    no_effect(&rows, invalid, "invalid_arguments");
    success(&rows, rotated);
    assert_eq!(
        retained(&rows, rotated, "oauth_refresh:first")["generation"],
        2
    );
    assert_eq!(
        retained(&rows, rotated, "oauth_refresh:first")["extra"],
        json!({"retained":true})
    );
    code(&rows, reused, "unauthenticated");
    assert_eq!(
        retained(&rows, reused, "oauth_refresh:first")["revoked"],
        true
    );
    no_effect(&rows, restart, "unauthenticated");
    no_effect(&rows, missing, "unauthenticated");
    for (index, key) in bindings {
        code(&rows, index, "unauthenticated");
        assert_eq!(
            retained(&rows, index, &format!("oauth_refresh:{key}"))["revoked"],
            true
        );
    }
}

#[test]
fn credential_configuration_commits_before_oauth_cleanup_and_preserves_root_and_active_bindings() {
    let mut steps = setup();
    let keys = steps.len();
    steps.push(action("oauth.initialize", json!({})));
    let alpha_ref = steps.len();
    steps.push(action(
        "credentialReference",
        json!({"digest":credential("a")}),
    ));
    let beta_ref = steps.len();
    steps.push(action(
        "credentialReference",
        json!({"digest":credential("b")}),
    ));
    for (id, from) in [("alpha", alpha_ref), ("beta", beta_ref)] {
        let mut value = create(id);
        bind_value(
            &mut value,
            "/params/state/credentialReference",
            from,
            "/value",
            false,
        );
        bind_value(
            &mut value,
            "/params/activeCredentialReferences/0",
            from,
            "/value",
            false,
        );
        // Keep both real bindings active while creating the second family.
        value["params"]["activeCredentialReferences"]
            .as_array_mut()
            .unwrap()
            .push(json!("bound"));
        bind_value(
            &mut value,
            "/params/activeCredentialReferences/1",
            alpha_ref,
            "/value",
            false,
        );
        steps.push(value);
    }
    let configured = steps.len();
    steps.push(action(
        "credentials.configure",
        json!({"credentials":[{"principalId":"beta","digest":credential("b")}]}),
    ));
    steps.push(sql("INSERT INTO metadata VALUES('oauth_refresh:bad','{');"));
    let failed = steps.len();
    steps.push(action("credentials.configure",json!({"credentials":[{"principalId":"alpha","digest":credential("c")},{"principalId":"beta","digest":credential("b")}]})));
    let authenticated = steps.len();
    steps.push(action(
        "authenticate",
        json!({"context":{"credentialDigest":credential("c")}}),
    ));
    steps.push(sql("DELETE FROM metadata WHERE key='oauth_refresh:bad'; UPDATE metadata SET value=json_set(value,'$.revoked',json('true')) WHERE key='oauth_refresh:beta'; CREATE TRIGGER reject_cleanup BEFORE DELETE ON metadata WHEN OLD.key LIKE 'oauth_refresh:%' BEGIN SELECT RAISE(ABORT,'fixture_cleanup'); END;"));
    let deletion = steps.len();
    steps.push(action(
        "credentials.configure",
        json!({"credentials":[{"principalId":"alpha","digest":credential("d")}]}),
    ));
    let active = steps.len();
    steps.push(action(
        "authenticate",
        json!({"context":{"credentialDigest":credential("d")}}),
    ));
    steps.push(sql("DROP TRIGGER reject_cleanup;"));
    let stable = steps.len();
    steps.push(action("oauth.initialize", json!({})));
    steps.push(sql(
        "UPDATE metadata SET value='invalid-root' WHERE key='cursor_secret';",
    ));
    let empty = steps.len();
    steps.push(action("credentials.configure", json!({"credentials":[]})));
    let rows = compare(&steps);
    for i in [configured, authenticated, active, stable, empty] {
        success(&rows, i);
    }
    assert!(
        state(&rows, configured)["metadata"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["key"] != "oauth_refresh:alpha")
    );
    assert_eq!(
        retained(&rows, configured, "oauth_refresh:beta")["revoked"],
        false
    );
    code(&rows, failed, "storage_invalid");
    assert_ne!(
        state(&rows, failed)["credentials"],
        state(&rows, failed - 1)["credentials"]
    );
    code(&rows, deletion, "internal_error");
    assert_eq!(
        retained(&rows, deletion, "oauth_refresh:beta")["revoked"],
        true
    );
    assert_eq!(value(&rows, keys), value(&rows, stable));
}

#[test]
fn oauth_family_capacity_cleanup_legacy_states_and_sql_failures_keep_original_transaction_semantics()
 {
    let mut steps = setup();
    let encoded = family("seed").to_string().replace('\'', "''");
    steps.push(sql(format!("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<1024) INSERT INTO metadata SELECT 'oauth_refresh:'||i,'{encoded}' FROM n; INSERT INTO metadata VALUES('oauthXrefresh:expired','{{\"expiresAt\":0}}');")));
    let full = steps.len();
    steps.push(create("new"));
    steps.push(sql(
        "UPDATE metadata SET value=json_set(value,'$.expiresAt',0) WHERE key='oauth_refresh:1';",
    ));
    let admitted = steps.len();
    steps.push(create("new"));
    let rows = compare(&steps);
    no_effect(&rows, full, "limit_exceeded");
    success(&rows, admitted);
    assert!(
        state(&rows, admitted)["metadata"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["key"] != "oauthXrefresh:expired")
    );

    let mut steps = setup();
    steps.push(sql("INSERT INTO metadata VALUES('oauth_refresh:expired','{\"expiresAt\":0}'); CREATE TRIGGER reject_family BEFORE INSERT ON metadata WHEN NEW.key='oauth_refresh:new' BEGIN SELECT RAISE(ABORT,'fixture_family'); END;"));
    let failed = steps.len();
    steps.push(create("new"));
    steps.push(sql("DROP TRIGGER reject_family;"));
    let retried = steps.len();
    steps.push(create("new"));
    steps.push(sql("CREATE TRIGGER reject_family BEFORE UPDATE ON metadata WHEN OLD.key='oauth_refresh:new' BEGIN SELECT RAISE(ABORT,'fixture_family'); END;"));
    let revocation = steps.len();
    let mut wrong = rotation("new");
    wrong["params"]["rotation"]["presentedTokenDigest"] = "wrong".into();
    steps.push(wrong);
    steps.push(sql("DROP TRIGGER reject_family;"));
    let rotated = steps.len();
    steps.push(rotation("new"));
    let rows = compare(&steps);
    no_effect(&rows, failed, "internal_error");
    success(&rows, retried);
    no_effect(&rows, revocation, "internal_error");
    success(&rows, rotated);
    for (raw, expected, revokes) in [
        ("{", "internal_error", false),
        ("null", "internal_error", false),
        ("5", "unauthenticated", true),
        ("[]", "unauthenticated", true),
        ("[1,{\"retained\":true}]", "unauthenticated", true),
        ("\"xy\"", "unauthenticated", true),
    ] {
        let mut steps = setup();
        steps.push(sql(format!(
            "INSERT INTO metadata VALUES('oauth_refresh:legacy','{}');",
            raw.replace('\'', "''")
        )));
        let rotate = steps.len();
        steps.push(rotation("legacy"));
        let rows = compare(&steps);
        code(&rows, rotate, expected);
        if revokes {
            assert_eq!(
                retained(&rows, rotate, "oauth_refresh:legacy")["revoked"],
                true
            );
        } else {
            assert_eq!(state(&rows, rotate), state(&rows, rotate - 1));
        }
    }
    for raw in ["{", "null", "[]", "5"] {
        let mut steps = setup();
        steps.push(sql(format!(
            "INSERT INTO metadata VALUES('oauth_refresh:legacy','{raw}');"
        )));
        let collect = steps.len();
        steps.push(action("collect", json!({})));
        let rows = compare(&steps);
        no_effect(&rows, collect, "storage_invalid");
    }
}
