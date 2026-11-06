#[path = "../fixtures/chat-native-driver-client.rs"]
mod native_client;
#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use ivy_chat_bridge::{
    admission::ChatAdmission, native_driver::ChatNativeDriver, store::ChatStore,
};
use ivy_contracts::{
    IvyJson as Json,
    checked_native_contract::CheckedNativeContracts,
    hash::hash_json,
    json::parse,
    native_backend::{BackendIdentity, NativeProvider},
};
use ivy_sdk::tools::ToolClients;
use native_client::Client;
use std::sync::{Arc, Mutex};

fn backend_fixture(catalogs: &CheckedNativeContracts) -> (Json, BackendIdentity) {
    let bytes = reference_data::read(include_bytes!("fixtures/chat-native-driver-cases.json.gz"));
    let rows = parse(std::str::from_utf8(&bytes).unwrap(), usize::MAX).unwrap();
    let mut row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row.get("label") == Some(&"ready".into()))
        .unwrap()
        .clone();
    let backend = BackendIdentity::new(
        "codex-work".to_owned(),
        NativeProvider::Codex,
        "work".to_owned(),
    )
    .unwrap();
    let definition = row.get_mut("definition").unwrap();
    definition
        .get_mut("project")
        .unwrap()
        .insert("namespace", backend.id().into());
    let plan = definition.get_mut("nativePlan").unwrap();
    let contract = catalogs
        .checked_native_contract(
            plan.required("nativeVersion")
                .unwrap()
                .as_js_string()
                .unwrap(),
            Some(&Json::object([(
                "sourceHash",
                plan.required("catalogSourceHash").unwrap().clone(),
            )])),
        )
        .unwrap()
        .with_backend(Some(backend.clone()));
    plan.insert("backend", backend.json());
    plan.get_mut("turnStart").unwrap().insert(
        "reservedFields",
        Json::Array(
            ["clientUserMessageId", "input", "threadId"]
                .into_iter()
                .map(Json::from)
                .collect(),
        ),
    );
    for (key, method) in [
        ("threadStart", "thread/start"),
        ("threadResume", "thread/resume"),
        ("turnStart", "turn/start"),
        ("turnInterrupt", "turn/interrupt"),
    ] {
        plan.get_mut("definitions").unwrap().insert(
            key,
            contract.definition_hash(&method.into()).unwrap().into(),
        );
    }
    row.get_mut("status")
        .unwrap()
        .insert("backend", backend.json());
    (row, backend)
}

#[tokio::test]
async fn configured_backend_routes_management_and_rejects_foreign_or_missing_account() {
    let catalogs = CheckedNativeContracts::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
    );
    for status_scope in ["selected", "other", "missing"] {
        let (mut row, backend) = backend_fixture(&catalogs);
        if status_scope == "other" {
            let mut other = backend.json();
            other.insert("accountId", "other".into());
            row.get_mut("status").unwrap().insert("backend", other);
        } else if status_scope == "missing" {
            row.get_mut("status").unwrap().remove("backend");
        }
        let client = Arc::new(Client {
            row: row.clone(),
            calls: Mutex::new(vec![]),
        });
        let admission = ChatAdmission::new(row.required("definition").unwrap()).unwrap();
        let store = ChatStore::open(client.clone(), None, None).await.unwrap();
        let instructions = "".into();
        let driver = ChatNativeDriver::new(
            &store,
            Arc::new(ToolClients::new(client.clone())),
            &admission,
            &catalogs,
            "chat-provider".into(),
            &instructions,
        )
        .unwrap();
        assert_eq!(
            driver.native_owner().unwrap().target().backend.as_ref(),
            Some(&backend)
        );
        let ready = driver.ready().await;
        if status_scope != "selected" {
            assert_eq!(ready.unwrap_err().code, "chat_native_unavailable");
            continue;
        }
        assert_eq!(ready.unwrap().get("backend"), Some(&backend.json()));
        driver
            .management("custom", Json::object([("original", "payload".into())]))
            .await
            .unwrap();
        for call in client
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.get("method") == Some(&"tools.call".into()))
        {
            assert_eq!(
                call.required("params")
                    .unwrap()
                    .required("arguments")
                    .unwrap()
                    .get("backend"),
                Some(&backend.json())
            );
        }
        let before = client.calls.lock().unwrap().len();
        let mut other = backend.json();
        other.insert("accountId", "other".into());
        assert_eq!(
            driver
                .management("custom", Json::object([("backend", other)]))
                .await
                .unwrap_err()
                .code,
            "native_backend_mismatch"
        );
        assert_eq!(
            client.calls.lock().unwrap().len(),
            before,
            "A foreign account must fail before RPC"
        );
        let mut wrong_namespace = row.required("definition").unwrap().clone();
        wrong_namespace
            .get_mut("project")
            .unwrap()
            .insert("namespace", "codex".into());
        assert_eq!(
            ChatAdmission::new(&wrong_namespace).err().unwrap().code,
            "chat_definition_mismatch"
        );
    }
}

#[tokio::test]
async fn configured_backend_plan_requests_and_read_evidence_preserve_exact_account() {
    use ivy_chat_bridge::{
        native_evidence::{ChatNativeEvidence, verify_chat_native_request},
        native_plan::{ChatNativeValidation, resolve_chat_plan},
    };
    let catalogs = CheckedNativeContracts::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
    );
    let (row, backend) = backend_fixture(&catalogs);
    let definition = row.required("definition").unwrap();
    let client = Arc::new(Client {
        row: row.clone(),
        calls: Mutex::new(vec![]),
    });
    let store = ChatStore::open(client.clone(), None, None).await.unwrap();
    let instructions = "".into();
    let validation = ChatNativeValidation::new(&catalogs, &instructions);
    let plan = resolve_chat_plan(
        &store,
        &catalogs,
        definition.required("nativePlan").unwrap(),
        &instructions,
    )
    .unwrap();
    assert_eq!(plan.get("backend"), Some(&backend.json()));
    let request = Json::object([
        ("backend", backend.json()),
        (
            "nativeVersion",
            plan.required("nativeVersion").unwrap().clone(),
        ),
        (
            "catalogSourceHash",
            plan.required("catalogSourceHash").unwrap().clone(),
        ),
        ("method", "thread/start".into()),
        ("params", plan.required("threadStart").unwrap().clone()),
    ]);
    let at = Json::from("2026-01-02T03:04:05.006Z");
    let call = Json::object([
        ("schemaVersion", 1.0.into()),
        ("operationId", "original-start".into()),
        ("serviceNodeId", "native-agent".into()),
        ("callerPrincipalId", "chat-service".into()),
        (
            "request",
            Json::object([
                ("objectId", "main-operation".into()),
                ("revision", 1.0.into()),
            ]),
        ),
        (
            "nativeVersion",
            plan.required("nativeVersion").unwrap().clone(),
        ),
        (
            "catalogSourceHash",
            plan.required("catalogSourceHash").unwrap().clone(),
        ),
        ("method", "thread/start".into()),
        ("predecessor", Json::Null),
        ("preparedEpoch", Json::Null),
        (
            "requestHash",
            hash_json(&Json::object([
                ("method", request.required("method").unwrap().clone()),
                ("params", request.required("params").unwrap().clone()),
            ]))
            .unwrap()
            .into(),
        ),
        (
            "expectedDefinitionHash",
            plan.required("definitions")
                .unwrap()
                .required("threadStart")
                .unwrap()
                .clone(),
        ),
        ("createdAt", at.clone()),
        ("updatedAt", at.clone()),
        ("state", "prepared".into()),
        ("epoch", Json::Null),
        ("evidence", Json::Null),
        ("code", Json::Null),
    ]);
    verify_chat_native_request(
        &store,
        definition,
        &call,
        &request,
        &catalogs,
        &validation,
        &instructions,
    )
    .await
    .unwrap();
    let mut foreign_request = request.clone();
    foreign_request
        .get_mut("backend")
        .unwrap()
        .insert("accountId", "other".into());
    assert_eq!(
        verify_chat_native_request(
            &store,
            definition,
            &call,
            &foreign_request,
            &catalogs,
            &validation,
            &instructions
        )
        .await
        .unwrap_err()
        .code,
        "chat_native_mismatch"
    );
    let params = Json::object([("threadId", "main".into()), ("limit", 1.0.into())]);
    let expected = Json::object([
        ("backend", backend.json()),
        ("serviceNodeId", "native-agent".into()),
        ("callerPrincipalId", "chat-service".into()),
        (
            "nativeVersion",
            plan.required("nativeVersion").unwrap().clone(),
        ),
        ("epoch", "epoch".into()),
        ("method", "thread/turns/list".into()),
        ("params", params.clone()),
    ]);
    let status = row.required("status").unwrap();
    let observed = Json::object([
        ("schemaVersion", 1.0.into()),
        ("observationId", "original-read".into()),
        ("backend", backend.json()),
        ("serviceNodeId", "native-agent".into()),
        ("callerPrincipalId", "chat-service".into()),
        ("epoch", "epoch".into()),
        ("requestId", "read-id".into()),
        ("observedAt", at),
        (
            "nativeVersion",
            plan.required("nativeVersion").unwrap().clone(),
        ),
        (
            "nativeExecutableHash",
            status.required("nativeExecutableHash").unwrap().clone(),
        ),
        (
            "catalogHash",
            status.required("catalogHash").unwrap().clone(),
        ),
        (
            "requestHash",
            hash_json(&Json::object([
                ("method", expected.required("method").unwrap().clone()),
                ("params", params.clone()),
            ]))
            .unwrap()
            .into(),
        ),
        ("method", "thread/turns/list".into()),
        ("params", params),
        (
            "reply",
            Json::object([(
                "result",
                Json::object([("data", Json::Array(vec![])), ("nextCursor", Json::Null)]),
            )]),
        ),
    ]);
    let evidence =
        ChatNativeEvidence::new(&store, definition, &catalogs, &validation, &instructions).unwrap();
    evidence.verify_read(&observed, &expected).unwrap();
    for missing in [false, true] {
        let mut foreign = observed.clone();
        if missing {
            foreign.remove("backend");
        } else {
            foreign
                .get_mut("backend")
                .unwrap()
                .insert("accountId", "other".into());
        }
        assert_eq!(
            evidence.verify_read(&foreign, &expected).unwrap_err().code,
            "chat_native_mismatch"
        );
    }
    let mut foreign = observed.clone();
    let mut foreign_expected = expected.clone();
    foreign
        .get_mut("backend")
        .unwrap()
        .insert("accountId", "other".into());
    foreign_expected
        .get_mut("backend")
        .unwrap()
        .insert("accountId", "other".into());
    assert_eq!(
        evidence
            .verify_read(&foreign, &foreign_expected)
            .unwrap_err()
            .code,
        "chat_native_mismatch",
        "Matching foreign evidence still does not belong to this configured ChatBridge"
    );
    assert!(
        client.calls.lock().unwrap().is_empty(),
        "Evidence verification must not dispatch native actions"
    );
}
