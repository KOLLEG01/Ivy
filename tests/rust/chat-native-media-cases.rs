use base64::{Engine as _, engine::general_purpose::STANDARD};
use ivy_chat_bridge::whatsapp::{
    audio_worker::{AudioWorker, decode_ogg_opus},
    media::{MediaDependencies, TranscriptionRequest},
    messages::Translator,
    native_media::NativeMediaDependencies,
};
use ivy_contracts::{IvyJson as Json, json::parse};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use wacore::{
    download::MediaType,
    net::{HttpClient, HttpRequest, HttpResponse, StreamingHttpResponse},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("ivy-native-media-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn worker() -> AudioWorker {
    AudioWorker::new(PathBuf::from(env!("CARGO_BIN_EXE_ivy-chat-bridge")))
}
#[derive(Default)]
struct MediaHttp {
    bytes: Mutex<Vec<u8>>,
    urls: Mutex<Vec<String>>,
    delay: bool,
}
#[async_trait::async_trait]
impl HttpClient for MediaHttp {
    async fn execute(&self, request: HttpRequest) -> whatsapp_rust::anyhow::Result<HttpResponse> {
        self.urls.lock().unwrap().push(request.url);
        if self.delay {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Ok(HttpResponse {
            status_code: 200,
            body: self.bytes.lock().unwrap().clone(),
        })
    }
    fn supports_streaming(&self) -> bool {
        !self.delay
    }
    fn execute_streaming(
        &self,
        request: HttpRequest,
    ) -> whatsapp_rust::anyhow::Result<StreamingHttpResponse> {
        self.urls.lock().unwrap().push(request.url);
        Ok(StreamingHttpResponse {
            status_code: 200,
            body: Box::new(std::io::Cursor::new(self.bytes.lock().unwrap().clone())),
        })
    }
}
fn dependencies(http: Arc<MediaHttp>) -> NativeMediaDependencies {
    NativeMediaDependencies::with_http(worker(), http, reqwest::Client::builder()).unwrap()
}
fn encrypted(http: &MediaHttp, bytes: &[u8], kind: MediaType) -> Json {
    let enc = wacore::upload::encrypt_media(bytes, kind).unwrap();
    *http.bytes.lock().unwrap() = enc.data_to_upload;
    Json::object([
        (
            "url",
            "https://example.invalid/original?signature=synthetic".into(),
        ),
        ("directPath", "/original?signature=synthetic".into()),
        ("thumbnailDirectPath", "/wrong-thumbnail".into()),
        ("mediaKey", STANDARD.encode(enc.media_key).into()),
    ])
}
#[tokio::test]
async fn native_media_downloads_only_the_original_and_authenticates_before_exposing_bytes() {
    let http = Arc::new(MediaHttp::default());
    let dependencies = dependencies(http.clone());
    let mut original = encrypted(&http, b"Synthetic original", MediaType::Image);
    let mut stream = dependencies
        .download(&original, "image", 1000)
        .await
        .unwrap();
    assert_eq!(stream.next().await.unwrap().unwrap(), b"Synthetic original");
    assert!(stream.next().await.unwrap().is_none());
    stream.destroy();
    assert_eq!(
        *http.urls.lock().unwrap(),
        ["https://example.invalid/original?signature=synthetic"]
    );
    original.remove("url");
    let mut stream = dependencies
        .download(&original, "image", 1000)
        .await
        .unwrap();
    stream.destroy();
    assert_eq!(
        http.urls.lock().unwrap().last().unwrap(),
        "https://mmg.whatsapp.net/original?signature=synthetic"
    );
    let last = http.bytes.lock().unwrap().len() - 1;
    http.bytes.lock().unwrap()[last] ^= 1;
    let error = dependencies
        .download(&original, "image", 1000)
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "whatsapp_media_invalid");
    assert!(!error.message.contains("signature"));
    let oversized = encrypted(&http, &vec![17; 4 * 1024 * 1024 + 1], MediaType::Image);
    assert_eq!(
        dependencies
            .download(&oversized, "image", 5000)
            .await
            .err()
            .unwrap()
            .code,
        "whatsapp_media_limit"
    );
    let unknown = dependencies
        .download(&original, "video", 1000)
        .await
        .err()
        .unwrap();
    assert_eq!(unknown.code, "whatsapp_media_invalid");
}
#[tokio::test]
async fn native_media_download_deadline_cancels_an_unfinished_http_exchange() {
    let http = Arc::new(MediaHttp {
        delay: true,
        ..Default::default()
    });
    let original = encrypted(&http, b"deadline", MediaType::Audio);
    let dependencies = dependencies(http);
    assert_eq!(
        dependencies
            .download(&original, "audio", 10)
            .await
            .err()
            .unwrap()
            .code,
        "whatsapp_media_invalid"
    );
}

struct TlsServer {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    origin: String,
}
impl TlsServer {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let mut child = Command::new("node")
            .current_dir(root)
            .arg("tests/rust/fixtures/chat-native-media-server.mjs")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let started = parse(line.trim(), usize::MAX).unwrap();
        let port = started.required("port").unwrap().as_f64().unwrap() as u16;
        Self {
            child,
            input,
            output,
            origin: format!("https://127.0.0.1:{port}"),
        }
    }
    fn stats(&mut self) -> Json {
        self.input.write_all(b"stats\n").unwrap();
        self.input.flush().unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        parse(line.trim(), usize::MAX).unwrap()
    }
    fn request(&self, path: &str, timeout_ms: u64) -> TranscriptionRequest {
        TranscriptionRequest {
            url: format!("{}{path}", self.origin),
            authorization: "Bearer fixture-token".into(),
            wav: b"RIFF fixture WAV".to_vec(),
            model: "fixture-model".into(),
            language: Some("de".into()),
            timeout_ms,
            redirect_error: true,
        }
    }
}
impl Drop for TlsServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
#[tokio::test]
async fn native_transcription_keeps_https_form_fields_redirect_refusal_and_body_deadlines() {
    let mut server = TlsServer::new();
    let certificate =
        reqwest::Certificate::from_pem(include_bytes!("fixtures/chat-media-test-cert.pem"))
            .unwrap();
    let dependencies = NativeMediaDependencies::with_http(
        worker(),
        Arc::new(MediaHttp::default()),
        reqwest::Client::builder()
            .add_root_certificate(certificate)
            .redirect(reqwest::redirect::Policy::limited(5)),
    )
    .unwrap();
    let mut response = dependencies
        .transcribe(server.request("/ok", 1000))
        .await
        .unwrap();
    assert!(response.ok());
    assert_eq!(
        response.json().await.unwrap().required("text").unwrap(),
        &Json::from("Local transcript")
    );
    let redirected = dependencies
        .transcribe(server.request("/redirect", 1000))
        .await
        .unwrap();
    assert!(!redirected.ok());
    let unavailable = dependencies
        .transcribe(server.request("/unavailable", 1000))
        .await
        .unwrap();
    assert!(!unavailable.ok());
    for path in ["/invalid-json", "/oversized"] {
        let mut response = dependencies
            .transcribe(server.request(path, 1000))
            .await
            .unwrap();
        assert_eq!(
            response.json().await.unwrap_err().code,
            "whatsapp_transcription_failed"
        );
    }
    assert_eq!(
        dependencies
            .transcribe(server.request("/slow", 20))
            .await
            .err()
            .unwrap()
            .code,
        "whatsapp_transcription_failed"
    );
    let mut slow_body = dependencies
        .transcribe(server.request("/slow-body", 100))
        .await
        .unwrap();
    assert_eq!(
        slow_body.json().await.unwrap_err().code,
        "whatsapp_transcription_failed"
    );
    let mut insecure = server.request("/ok", 1000);
    insecure.url = insecure.url.replacen("https:", "http:", 1);
    assert_eq!(
        dependencies.transcribe(insecure).await.err().unwrap().code,
        "whatsapp_transcription_failed"
    );
    let stats = server.stats();
    let stats = stats.as_array().unwrap();
    assert!(!stats.iter().any(|r| r.get("path") == Some(&"/sink".into())));
    let sent = &stats[0];
    assert_eq!(
        sent.get("authorization"),
        Some(&"Bearer fixture-token".into())
    );
    assert_eq!(sent.get("method"), Some(&"POST".into()));
    let content_type = sent.required("contentType").unwrap().as_str().unwrap();
    assert!(content_type.starts_with("multipart/form-data; boundary="));
    let body = String::from_utf8(
        STANDARD
            .decode(sent.required("body").unwrap().as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    for field in [
        "name=\"file\"; filename=\"audio.wav\"",
        "Content-Type: audio/wav",
        "RIFF fixture WAV",
        "name=\"model\"",
        "fixture-model",
        "name=\"response_format\"",
        "json",
        "name=\"language\"",
        "de",
    ] {
        assert!(body.contains(field), "Missing form field {field}: {body}");
    }
}

fn opus_file(
    channels: u8,
    gain: i16,
    pre_skip: u16,
    trim: u64,
    packets: usize,
    serial: u32,
) -> Vec<u8> {
    use ogg::writing::{
        PacketWriteEndInfo::{EndPage, EndStream},
        PacketWriter,
    };
    let mut writer = PacketWriter::new(Vec::new());
    let mut header = b"OpusHead".to_vec();
    header.extend([1, channels]);
    header.extend(pre_skip.to_le_bytes());
    header.extend(48000u32.to_le_bytes());
    header.extend(gain.to_le_bytes());
    header.push(0);
    writer.write_packet(header, serial, EndPage, 0).unwrap();
    let mut tags = b"OpusTags".to_vec();
    tags.extend(0u32.to_le_bytes());
    tags.extend(0u32.to_le_bytes());
    writer.write_packet(tags, serial, EndPage, 0).unwrap();
    let mut encoder = opus::Encoder::new(
        48000,
        if channels == 1 {
            opus::Channels::Mono
        } else {
            opus::Channels::Stereo
        },
        opus::Application::Audio,
    )
    .unwrap();
    let samples = (0..960 * channels as usize)
        .map(|i| ((i / channels as usize) as f32 * 0.07).sin() * 0.2)
        .collect::<Vec<_>>();
    let mut packet = vec![0; 4096];
    let repeated = if packets > 100 {
        let count = encoder.encode_float(&samples, &mut packet).unwrap();
        Some(packet[..count].to_vec())
    } else {
        None
    };
    for i in 0..packets {
        let encoded = if let Some(packet) = &repeated {
            packet.clone()
        } else {
            let count = encoder.encode_float(&samples, &mut packet).unwrap();
            packet[..count].to_vec()
        };
        writer
            .write_packet(
                encoded,
                serial,
                if i + 1 == packets { EndStream } else { EndPage },
                ((i + 1) * 960) as u64 - if i + 1 == packets { trim } else { 0 },
            )
            .unwrap();
    }
    writer.into_inner()
}
#[tokio::test]
async fn native_opus_worker_matches_original_duration_gain_and_pcm_and_rejects_incomplete_data() {
    let temp = Temp::new();
    // Synthetic mono/stereo Opus with gain, pre-skip and end trimming; WAVs
    // were captured with the original codec. No JavaScript backend is loaded.
    for (bytes, expected) in [
        (
            include_bytes!("fixtures/chat-opus-1.ogg").as_slice(),
            include_bytes!("fixtures/chat-opus-1.wav").as_slice(),
        ),
        (
            include_bytes!("fixtures/chat-opus-2.ogg").as_slice(),
            include_bytes!("fixtures/chat-opus-2.wav").as_slice(),
        ),
    ] {
        let wav = worker()
            .decode(&bytes, Translator::default())
            .await
            .unwrap();
        let mut process = Command::new(env!("CARGO_BIN_EXE_ivy-chat-bridge"))
            .arg(ivy_chat_bridge::whatsapp::audio_worker::WORKER_ARGUMENT)
            .env("PATH", "")
            .env("IVY_INSTANCE_CONFIG", temp.0.join("unused-instance.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        process.stdin.take().unwrap().write_all(&bytes).unwrap();
        let direct = process.wait_with_output().unwrap();
        assert!(direct.status.success());
        assert_eq!(
            direct.stdout, wav,
            "Native worker with no Node, PATH or instance configuration"
        );
        assert_eq!(
            &wav[..44],
            &expected[..44],
            "Native/original duration, channels and format"
        );
        let native: Vec<i16> = wav[44..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes(b.try_into().unwrap()))
            .collect();
        let original: Vec<i16> = expected[44..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes(b.try_into().unwrap()))
            .collect();
        let maximum = native
            .iter()
            .zip(&original)
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap();
        assert!(
            maximum <= 4,
            "Native/original PCM maximum difference: {maximum}"
        );
        assert!(
            worker()
                .decode(&bytes[..bytes.len() - 10], Translator::default())
                .await
                .is_err()
        );
        let mut corrupt = bytes.to_vec();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 1;
        assert!(decode_ogg_opus(&corrupt).is_err());
    }
    let bytes = opus_file(1, 0, 312, 137, 2, 19);
    let result = AudioWorker::with_timeout(
        PathBuf::from(env!("CARGO_BIN_EXE_ivy-chat-bridge")),
        Duration::ZERO,
    )
    .decode(&bytes, Translator::new(Some(&"de".into())))
    .await
    .unwrap_err();
    assert_eq!(result.code, "whatsapp_audio_timeout");
    assert_eq!(
        result.message,
        Translator::new(Some(&"de".into()))
            .translate("audio.timeout", None)
            .unwrap()
            .to_utf8()
            .unwrap()
    );
    assert!(decode_ogg_opus(&[]).is_err());
}
#[test]
fn native_opus_decoder_bounds_recording_duration_and_keeps_chained_streams() {
    let mut bytes = opus_file(1, 0, 312, 137, 3, 11);
    bytes.extend(opus_file(2, 0, 312, 137, 4, 12));
    let wav = decode_ogg_opus(&bytes).unwrap();
    assert_eq!(
        wav.len(),
        44 + ((2880usize - 312 - 137).div_ceil(3) + (3840usize - 312 - 137).div_ceil(3)) * 2
    );
    let too_long = opus_file(1, 0, 0, 0, 30_001, 1);
    assert!(decode_ogg_opus(&too_long).is_err());
}

async fn retained_account(root: &Path, number: &str) {
    use ivy_chat_bridge::whatsapp::{auth::AuthAccount, auth_backend::NativeAuthStore};
    use wacore::store::traits::DeviceStore;
    std::fs::create_dir_all(root.join("whatsapp")).unwrap();
    std::fs::create_dir(root.join("whatsapp-owner")).unwrap();
    // Original Node ExecutorLock takes a directory, creates this database, and
    // leaves its PID row behind after closing the kernel lock.
    let node_lock =
        rusqlite::Connection::open(root.join("whatsapp-owner/executor-lock.sqlite")).unwrap();
    node_lock
        .execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; CREATE TABLE owner(pid INTEGER); INSERT INTO owner VALUES (1); COMMIT;")
        .unwrap();
    node_lock.close().unwrap();
    let backend = NativeAuthStore::new(
        AuthAccount::acquire(
            &root.join("whatsapp/auth.sqlite"),
            &root.join("whatsapp-owner/executor-lock.sqlite"),
        )
        .unwrap(),
    );
    backend.create().await.unwrap();
    let mut device = backend.load().await.unwrap().unwrap();
    device.pn = Some(format!("{number}:1@s.whatsapp.net").parse().unwrap());
    backend.save(&device).await.unwrap();
    backend.close().unwrap();
}
fn account_config() -> Json {
    parse(r#"{"accountId":"synthetic-account","botPhoneNumber":"+15555550102","since":"2020-01-01T00:00:00.000Z","senders":[{"phoneNumber":"+15555550101","principalId":"synthetic-principal","channelId":"synthetic-channel"}]}"#,usize::MAX).unwrap()
}
#[tokio::test]
async fn native_service_parts_open_original_node_owner_layout_without_network_and_release_the_account()
 {
    use ivy_chat_bridge::whatsapp::{journal_io::JournalIo, journal_owner::Journal, service_parts};
    let root = Temp::new();
    retained_account(&root.0, "15555550102").await;
    let lock_path = root.0.join("whatsapp-owner/executor-lock.sqlite");
    let original_lock = std::fs::read(&lock_path).unwrap();
    let original = Journal::new(JournalIo::open(root.0.join("whatsapp.sqlite")).unwrap());
    let ids = original
        .access(|j| {
            j.enqueue(
                &"old-output".into(),
                "15555550101@s.whatsapp.net",
                &"Retained text".into(),
                false,
                1,
            )
        })
        .unwrap();
    original.close().await.unwrap();
    let parts = service_parts::open(
        account_config(),
        root.0.clone(),
        root.0.join("artifact"),
        PathBuf::from(env!("CARGO_BIN_EXE_ivy-chat-bridge")),
        Translator::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        parts.socket.state(),
        ivy_contracts::JsString::from("stopped")
    );
    assert_eq!(
        parts.journal.access(|j| j.outgoing()).unwrap()[0].id,
        ids[0]
    );
    assert_eq!(
        service_parts::open(
            account_config(),
            root.0.clone(),
            root.0.join("artifact"),
            PathBuf::from(env!("CARGO_BIN_EXE_ivy-chat-bridge")),
            Translator::default()
        )
        .await
        .err()
        .unwrap()
        .code,
        "executor_already_running"
    );
    (parts.resources.status)(Json::object([("state", "fixture".into())]))
        .await
        .unwrap();
    assert_eq!(
        parse(
            &std::fs::read_to_string(root.0.join("whatsapp-status.json")).unwrap(),
            usize::MAX
        )
        .unwrap()
        .get("state"),
        Some(&"fixture".into())
    );
    parts.socket.close().await.unwrap();
    (parts.resources.retire_account)().await.unwrap();
    parts.journal.close().await.unwrap();
    drop(parts);
    assert!(root.0.join("whatsapp-owner").is_dir());
    assert_eq!(std::fs::read(lock_path).unwrap(), original_lock);
    let next = service_parts::open(
        account_config(),
        root.0.clone(),
        root.0.join("artifact"),
        PathBuf::from(env!("CARGO_BIN_EXE_ivy-chat-bridge")),
        Translator::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        next.socket.state(),
        ivy_contracts::JsString::from("stopped")
    );
    next.socket.close().await.unwrap();
    (next.resources.retire_account)().await.unwrap();
    next.journal.close().await.unwrap();
}
#[tokio::test]
async fn native_service_parts_failed_account_admission_keeps_retained_auth_and_releases_ownership()
{
    use ivy_chat_bridge::whatsapp::{
        auth::{AuthAccount, AuthStore},
        service_parts,
    };
    let root = Temp::new();
    retained_account(&root.0, "15555550999").await;
    let before = AuthStore::open(root.0.join("whatsapp/auth.sqlite"))
        .unwrap()
        .get("creds", "default")
        .unwrap();
    assert_eq!(
        service_parts::open(
            account_config(),
            root.0.clone(),
            root.0.join("artifact"),
            PathBuf::from(env!("CARGO_BIN_EXE_ivy-chat-bridge")),
            Translator::default()
        )
        .await
        .err()
        .unwrap()
        .code,
        "whatsapp_account_mismatch"
    );
    let mut owner = AuthAccount::acquire(
        &root.0.join("whatsapp/auth.sqlite"),
        &root.0.join("whatsapp-owner/executor-lock.sqlite"),
    )
    .unwrap();
    assert_eq!(owner.store().get("creds", "default").unwrap(), before);
    owner.close().unwrap();
}
