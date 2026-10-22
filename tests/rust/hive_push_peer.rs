use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_hive::{browser_push_https::NativeHttps, browser_push_native::NativeSender};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdout, Command},
    time::{Instant, timeout},
};
use tokio_rustls::rustls::{
    ClientConfig, RootCertStore, crypto::aws_lc_rs, pki_types::CertificateDer,
};

pub struct Oracle {
    child: Child,
    output: Lines<BufReader<ChildStdout>>,
    pub profile: Value,
}
impl Oracle {
    pub async fn start() -> Self {
        Self::start_profile(None).await
    }
    pub async fn start_profile(profile: Option<&Value>) -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let mut command =
            Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()));
        if let Some(profile) = profile {
            command.env("IVY_HIVE_PUSH_PROFILE", profile.to_string());
        }
        let mut child = command
            .arg("tests/fixtures/hive-push-https-oracle.mjs")
            .current_dir(root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
        let profile = serde_json::from_str(
            &timeout(Duration::from_secs(10), output.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        Self {
            child,
            output,
            profile,
        }
    }
    pub async fn call(&mut self, input: Value) -> Value {
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(format!("{input}\n").as_bytes())
            .await
            .unwrap();
        serde_json::from_str(
            &timeout(Duration::from_secs(25), self.output.next_line())
                .await
                .unwrap()
                .unwrap()
                .expect("oracle exited"),
        )
        .unwrap()
    }
    pub async fn close(mut self) {
        assert_eq!(self.call(json!({"op":"close"})).await["closed"], true);
        assert!(
            timeout(Duration::from_secs(5), self.child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }
    pub fn sender(&self, trusted: bool) -> NativeSender {
        NativeSender::new(
            Arc::new(self.transport(trusted)),
            Arc::new(|| Ok(1700043200)),
        )
    }
    pub fn transport(&self, trusted: bool) -> NativeHttps {
        let mut roots = RootCertStore::empty();
        if trusted {
            roots
                .add(CertificateDer::from(
                    STANDARD
                        .decode(self.profile["caDer"].as_str().unwrap())
                        .unwrap(),
                ))
                .unwrap();
        }
        let config = ClientConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        NativeHttps::new(tokio::runtime::Handle::current(), Arc::new(config)).unwrap()
    }
}

pub fn options(profile: &Value) -> IvyJson {
    IvyJson::from_serde(&json!({"TTL":3600,"timeout":10000,"vapidDetails":profile["details"]}))
        .unwrap()
}
pub async fn send(
    sender: &NativeSender,
    profile: &Value,
    path: &str,
    host: Option<&str>,
) -> (u16, f64) {
    let mut sub = profile["subscription"].clone();
    let base = host.map_or_else(
        || profile["base"].as_str().unwrap().to_string(),
        |host| format!("https://{host}:{}", profile["port"]),
    );
    sub["endpoint"] = json!(format!("{base}{path}"));
    let start = Instant::now();
    let outcome = timeout(
        Duration::from_secs(23),
        sender.send(
            IvyJson::from_serde(&sub).unwrap(),
            "notification".into(),
            options(profile),
        ),
    )
    .await
    .unwrap();
    let status = match outcome {
        SendOutcome::Response(Some(IvyJson::Number(n)))
        | SendOutcome::Rejected(Some(IvyJson::Number(n))) => n as u16,
        SendOutcome::Rejected(None) => 0,
        _ => panic!("unexpected outcome"),
    };
    (status, start.elapsed().as_secs_f64() * 1000.0)
}
pub fn save(value: &Value) {
    if let Some(path) = std::env::var_os("IVY_HIVE_PUSH_HTTPS_CORPUS") {
        use std::fs::OpenOptions;
        // Format before writing: streaming Display pieces from parallel tests can
        // interleave even when each file handle uses append mode.
        let line = format!("{value}\n");
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
    }
}
pub fn semantic_headers(request: &Value) -> Value {
    let raw = request["rawHeaders"].as_array().unwrap();
    let mut fields = serde_json::Map::new();
    for pair in raw.chunks_exact(2) {
        let name = pair[0].as_str().unwrap().to_ascii_lowercase();
        assert!(
            fields.insert(name, pair[1].clone()).is_none(),
            "unexpected duplicate field"
        );
    }
    // Crypto randomness is verified in the existing cross-decryption oracle.
    // Here retain the whole raw request in the corpus and compare transport fields.
    assert!(
        fields
            .remove("authorization")
            .unwrap()
            .as_str()
            .unwrap()
            .starts_with("vapid t=")
    );
    Value::Object(fields)
}
