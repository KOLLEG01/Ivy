use super::*;
use ivy_contracts::{
    json::{parse, stringify},
    schema::SchemaValidators,
};
use std::{future::Future, pin::Pin, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

fn value(text: &str) -> IvyJson {
    parse(text, 4 * 1024 * 1024).unwrap()
}
fn empty() -> IvyJson {
    IvyJson::object([] as [(&str, IvyJson); 0])
}
fn catalog() -> IvyJson {
    value(include_str!(
        "../../specs/native/codex-0.154.0/catalog.json"
    ))
}
fn schema<'a>(catalog: &'a IvyJson, method: &IvyJson, direction: &str) -> &'a IvyJson {
    catalog
        .get("clientRequests")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d.get("method") == Some(method))
        .unwrap()
        .get(direction)
        .unwrap()
}
async fn timed<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(8), future)
        .await
        .unwrap()
}

#[test]
fn pinned_voice_documents_and_corrected_compiler_match_original() {
    let cases = value(include_str!("../fixtures/phone-voice-schema-cases.json"));
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/phone-voice-schemas-oracle.mjs",
        ])
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = value(std::str::from_utf8(&output.stdout).unwrap());
    let validators = SchemaValidators::new();
    let native = catalog();
    let mut actual = vec![];
    for (item, expected) in cases
        .as_array()
        .unwrap()
        .iter()
        .zip(expected.as_array().unwrap())
    {
        let direction = item
            .get("direction")
            .map(|v| v.as_str().unwrap())
            .unwrap_or_default();
        let definition = item
            .get("schema")
            .unwrap_or_else(|| schema(&native, item.get("method").unwrap(), &direction));
        let maximum = item
            .get("maximumBytes")
            .map(|v| v.as_f64().unwrap() as usize)
            .unwrap_or_else(|| {
                if direction == "inputSchema" {
                    6 * 1024 * 1024
                } else if item.get("method") == Some(&"model/list".into()) {
                    256 * 1024
                } else {
                    64 * 1024
                }
            });
        let result =
            VoiceSchemas::validate(&validators, definition, item.get("value").unwrap(), maximum);
        assert_eq!(
            result.is_ok(),
            item.get("valid") == Some(&true.into()),
            "{}",
            stringify(item.get("name").unwrap(), 1000).unwrap()
        );
        let mut fields = vec![
            ("name", item.get("name").unwrap().clone()),
            ("ok", result.is_ok().into()),
        ];
        if let Err(error) = result {
            fields.push(("code", IvyJson::String(error.code_js())));
            fields.push(("message", IvyJson::String(error.message_js())));
        }
        let row = IvyJson::object(fields);
        assert_eq!(&row, expected);
        actual.push(row);
    }
    assert_eq!(actual.len(), 38);
    if let Ok(path) = std::env::var("IVY_PHONE_VOICE_SCHEMAS_TRACE") {
        std::fs::write(
            path,
            stringify(
                &IvyJson::object([("original", expected), ("rust", IvyJson::Array(actual))]),
                4 * 1024 * 1024,
            )
            .unwrap(),
        )
        .unwrap();
    }
}

struct Peer {
    input: BufReader<DuplexStream>,
    output: DuplexStream,
}
impl Peer {
    async fn request(&mut self) -> IvyJson {
        let mut text = String::new();
        assert_ne!(timed(self.input.read_line(&mut text)).await.unwrap(), 0);
        value(&text)
    }
    async fn reply(&mut self, request: &IvyJson, result: IvyJson) {
        let frame = IvyJson::object([
            ("id", request.get("id").unwrap().clone()),
            ("result", result),
        ]);
        self.output
            .write_all(format!("{}\n", stringify(&frame, 4 * 1024 * 1024).unwrap()).as_bytes())
            .await
            .unwrap();
    }
}
fn connection() -> (VoiceRpc, Peer) {
    let (input, sent) = tokio::io::duplex(65536);
    let (emit, output) = tokio::io::duplex(65536);
    let rpc = VoiceRpc::start(
        catalog(),
        input,
        output,
        Events::default(),
        Arc::new(SchemaValidators::new()),
    )
    .unwrap();
    (
        rpc,
        Peer {
            input: BufReader::new(sent),
            output: emit,
        },
    )
}

#[tokio::test]
async fn real_compiler_rejects_input_before_dispatch_and_keeps_the_same_rpc_usable() {
    let (rpc, mut peer) = connection();
    let error = rpc
        .call(
            "thread/realtime/stop",
            value(r#"{"threadId":7}"#),
            Default::default(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    let current = rpc.clone();
    let request = tokio::spawn(async move {
        current
            .call(
                "thread/realtime/stop",
                value(r#"{"threadId":"t"}"#),
                Default::default(),
                None,
            )
            .await
    });
    let frame = peer.request().await;
    assert_eq!(
        frame.get("params"),
        Some(&value(r#"{"threadId":"t"}"#)),
        "No rejected input may reach the native actor"
    );
    peer.reply(&frame, empty()).await;
    assert_eq!(timed(request).await.unwrap().unwrap(), empty());
    assert!(rpc.transport().connected());
    rpc.transport().close("fixture_done").await;
}

struct Control(VoiceRpc);
impl VoiceModelControl for Control {
    fn open(&self) -> Pin<Box<dyn Future<Output = IvyResult<VoiceRpc>> + Send + '_>> {
        Box::pin(async { Ok(self.0.clone()) })
    }
    fn current(&self) -> Option<VoiceRpc> {
        Some(self.0.clone())
    }
}
impl VoiceSpeakerControl for Control {
    fn speaker_settings(&self) -> VoiceSpeakerSettings {
        VoiceSpeakerSettings {
            cwd: "C:\\work".into(),
            voice: None,
        }
    }
}
#[tokio::test]
async fn model_and_speaker_outputs_use_exact_pinned_documents_and_can_retry() {
    let (rpc, mut peer) = connection();
    let control = Arc::new(Control(rpc.clone()));
    let models = VoiceModelCatalog::new(control.clone());
    let speakers = VoiceSpeakerCatalog::new(control);
    for (page, valid) in [
        (value(r#"{"data":[{}]}"#), false),
        (value(r#"{"data":[],"nextCursor":null}"#), true),
    ] {
        let models = models.clone();
        let request = tokio::spawn(async move { models.models().await });
        let frame = peer.request().await;
        assert_eq!(frame.get("method"), Some(&"model/list".into()));
        peer.reply(&frame, page).await;
        let result = timed(request).await.unwrap();
        assert_eq!(result.is_ok(), valid);
        if !valid {
            assert_eq!(result.unwrap_err().code, "invalid_arguments");
        }
    }
    for (page, valid) in [
        (
            value(r#"{"voices":{"v1":["cove"],"defaultV1":"cove"}}"#),
            false,
        ),
        (
            value(
                r#"{"voices":{"v1":["cove"],"defaultV1":"cove","v2":["marin"],"defaultV2":"marin"}}"#,
            ),
            true,
        ),
    ] {
        let speakers = speakers.clone();
        let request = tokio::spawn(async move { speakers.speaker_voices().await });
        let frame = peer.request().await;
        assert_eq!(
            frame.get("method"),
            Some(&"thread/realtime/listVoices".into())
        );
        peer.reply(&frame, page).await;
        if valid {
            let frame = peer.request().await;
            assert_eq!(frame.get("method"), Some(&"config/read".into()));
            // The original speaker owner consumes config directly; it does not
            // add a config output-schema check to the shared call adapter.
            peer.reply(&frame, value(r#"{"config":{}}"#)).await;
        }
        let result = timed(request).await.unwrap();
        assert_eq!(result.is_ok(), valid);
        if !valid {
            assert_eq!(result.unwrap_err().code, "invalid_arguments");
        }
    }
    assert_eq!(models.models().await.unwrap(), IvyJson::Array(vec![]));
    assert_eq!(
        speakers
            .speaker_voices()
            .await
            .unwrap()
            .get("defaultVoiceId"),
        Some(&"cove".into())
    );
    rpc.transport().close("fixture_done").await;
}

// Reuse Shared's existing cache failure/eviction case, through Phone's required
// trait for the wrapper-admission assertion. The compiler stays Shared-owned.
#[test]
fn shared_cache_failure_bound_and_retained_handles_survive_phone_binding() {
    let validators = SchemaValidators::new();
    let schema = value(r#"{"type":"string"}"#);
    let first = validators.compile(&schema).unwrap();
    assert!(Arc::ptr_eq(&first, &validators.compile(&schema).unwrap()));
    for i in 0..255 {
        assert!(
            validators
                .compile(&IvyJson::object([
                    ("title", i.to_string().into()),
                    ("enum", IvyJson::Array(vec![]))
                ]))
                .is_err()
        );
    }
    assert!(Arc::ptr_eq(&first, &validators.compile(&schema).unwrap()));
    assert!(
        validators
            .compile(&IvyJson::object([("unknown", true.into())]))
            .is_err()
    );
    assert!(Arc::ptr_eq(&first, &validators.compile(&schema).unwrap()));
    validators.compile(&value(r#"{"type":"integer"}"#)).unwrap();
    assert!(!Arc::ptr_eq(&first, &validators.compile(&schema).unwrap()));
    assert!(first.matches(&"still usable".into()).unwrap());
    assert!(!first.matches(&IvyJson::Number(1.0)).unwrap());
    assert_eq!(
        VoiceSchemas::validate(
            &validators,
            &IvyJson::object([("unknown", true.into())]),
            &"too long".into(),
            2
        )
        .unwrap_err()
        .code,
        "content_too_large"
    );
}
