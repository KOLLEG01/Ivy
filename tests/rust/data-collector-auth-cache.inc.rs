use super::*;
use ivy_contracts::IvyResult;
use ivy_data_collector::{
    definitions::Definitions,
    engine::Engine,
    store::{CollectorStore, Limits},
    store_owner::StoreOwner,
};
use ivy_sdk::{
    clock::SystemClock,
    rpc::{RequestFuture, RequestOptions, RpcClient},
};

struct Peer(IvyJson);
impl RpcClient for Peer {
    fn request(&self, method: &str, params: IvyJson, _: RequestOptions) -> RequestFuture<'_> {
        let value = match method {
            "objects.read" => {
                assert_eq!(params.get("objectId"), Some(&"root".into()));
                parse(r#"{"object":{"id":"root","contractKey":"data-collector/root","effectivelyArchived":false}}"#, 1000).unwrap()
            }
            "objects.query" => {
                assert_eq!(
                    params.get("contractKey"),
                    Some(&"data-collector/assignment".into())
                );
                parse(r#"{"items":[{"document":{"object":{"id":"assignment"},"revision":{"revision":1},"content":{"encoding":"json","value":{"schemaVersion":1,"jobId":"job","definitionRevision":1,"serviceNodeId":"source.collector","enabled":false,"rootObjectId":"root","resultObjectId":null}}}}],"nextCursor":null}"#, 1000).unwrap()
            }
            "tools.list" => {
                assert_eq!(
                    params.get("serviceNodeId"),
                    Some(&"source.collector".into())
                );
                let mut item = parse(r#"{"qualifiedName":"data-collector.read","definition":{"interfaceVersion":"1.0.0","annotations":{"readOnlyHint":true}}}"#, 1000).unwrap();
                item.insert(
                    "definitionHash",
                    format!("sha256:{}", "a".repeat(64)).into(),
                );
                IvyJson::object([
                    (
                        "provider",
                        parse(r#"{"node":{"serviceNodeId":"source.collector"}}"#, 100).unwrap(),
                    ),
                    ("nextCursor", IvyJson::Null),
                    ("items", IvyJson::Array(vec![item])),
                ])
            }
            "tools.call" => {
                assert_eq!(
                    params.get("qualifiedName"),
                    Some(&"data-collector.read".into())
                );
                assert_eq!(
                    params.required("arguments").unwrap().get("view"),
                    Some(&"handoff".into())
                );
                self.0.clone()
            }
            _ => panic!("Unexpected handoff RPC {method}: {params:?}"),
        };
        Box::pin(async move { Ok(value) })
    }
}

#[tokio::test]
async fn equal_authentication_caches_have_portable_hashes_and_changed_bytes_reject_handoff() {
    let root = Root::new();
    let settings = parse(r#"{"rootObjectId":"root"}"#, 100).unwrap();
    let source = TaskRunner::new(
        root.0.join("source"),
        root.0.clone(),
        None,
        settings.clone(),
        None,
        None,
    )
    .await
    .unwrap();
    let target = Arc::new(
        TaskRunner::new(
            root.0.join("target"),
            root.0.clone(),
            None,
            settings.clone(),
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let source_auth = root.0.join("source/fixture/.auth");
    let target_auth = root.0.join("target/fixture/.auth");
    let files = [
        ("é", b"private-value".as_slice()),
        ("Z", b"Z".as_slice()),
        ("a", b"a".as_slice()),
        ("nested/token", &[0, 255, 13, 10]),
        ("Å", b"A".as_slice()),
    ];
    assert_eq!(source.authentication_fingerprint("fixture").unwrap(), None);
    std::fs::create_dir_all(&source_auth).unwrap();
    assert_eq!(
        source
            .authentication_fingerprint("fixture")
            .unwrap()
            .as_deref(),
        Some("sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
    );
    std::fs::create_dir(source_auth.join("nested")).unwrap();
    for (name, bytes) in files {
        std::fs::write(source_auth.join(name), bytes).unwrap();
    }
    let fingerprint = source
        .authentication_fingerprint("fixture")
        .unwrap()
        .unwrap();
    assert_eq!(fingerprint.len(), 71);
    assert!(fingerprint.starts_with("sha256:"));
    let payload = IvyJson::object([
        ("taskId", "fixture".into()),
        ("authenticationFingerprint", fingerprint.clone().into()),
        ("state", IvyJson::Null),
    ]);
    let store = StoreOwner::new(
        CollectorStore::open(
            &root.0.join("target.sqlite"),
            Limits::default(),
            "2026-01-01T00:00:00.000Z",
        )
        .unwrap(),
    );
    let engine = Engine::new(
        store.clone(),
        target.clone(),
        settings,
        Arc::new(SystemClock),
    )
    .await
    .unwrap();
    engine.attach(Arc::new(Peer(payload))).await.unwrap();
    let captured = target.clone();
    let definitions = Definitions::new(
        "target.collector".into(),
        engine,
        Arc::new(move |id| captured.authentication_fingerprint(id)),
    );
    async fn prepare(definitions: &Definitions, operation: &str) -> IvyResult<IvyJson> {
        definitions
            .update(
                IvyJson::object([
                    ("action", "prepareHandoff".into()),
                    ("jobId", "job".into()),
                    ("expectedRevision", IvyJson::Number(1.0)),
                    ("operationId", operation.into()),
                ]),
                None,
            )
            .await
    }
    assert_eq!(
        prepare(&definitions, "missing-cache")
            .await
            .unwrap_err()
            .code,
        "authentication_required"
    );
    assert!(!target_auth.exists()); // Handoff never copies private authentication data.
    assert!(
        store
            .call(|s| s.metadata("handoff-state:job"))
            .await
            .unwrap()
            .is_none()
    );
    std::fs::create_dir_all(target_auth.join("nested")).unwrap();
    for (name, bytes) in files.into_iter().rev() {
        std::fs::write(target_auth.join(name), bytes).unwrap();
    }
    assert_eq!(
        target.authentication_fingerprint("fixture").unwrap(),
        Some(fingerprint)
    );
    assert_eq!(
        prepare(&definitions, "equal-cache")
            .await
            .unwrap()
            .get("prepared"),
        Some(&"job".into())
    );
    let prepared = store
        .call(|s| s.metadata("handoff-state:job"))
        .await
        .unwrap()
        .unwrap();
    assert!(!prepared.contains("private-value"));
    std::fs::write(target_auth.join("é"), "changed-private-value").unwrap();
    assert_eq!(
        prepare(&definitions, "changed-cache")
            .await
            .unwrap_err()
            .code,
        "authentication_required"
    );
    assert_eq!(
        store
            .call(|s| s.metadata("handoff-state:job"))
            .await
            .unwrap(),
        Some(prepared)
    );
    store.close().await.unwrap();
}
