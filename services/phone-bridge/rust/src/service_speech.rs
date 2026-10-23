//! Protected announcement synthesis. A submitted request is never sent twice.
use crate::{
    journal::PhoneJournal,
    service_settings::{PhoneConfigurationSection, phone_configuration_section},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    hash::{digest, hash_json},
};
use ivy_host_runtime::node_path;
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    field(value, key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn require(ok: bool, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new("invalid_arguments", message))
    }
}
fn unknown() -> IvyError {
    IvyError::new(
        "phone_speech_unknown",
        "Original speech synthesis failed or has an unknown outcome; it was not repeated.",
    )
    .with_outcome(Outcome::Unknown)
}
fn speech_endpoint(settings: &IvyJson, announcement: &JsString) -> IvyResult<reqwest::Url> {
    let credentials = text(settings, "credentialsPath")?;
    require(
        node_path::is_absolute(node_path::Style::native(), &credentials)
            && !announcement.0.is_empty()
            && announcement.to_utf8()?.chars().count() <= 4096,
        "Screening needs speech settings and a bounded announcement.",
    )?;
    let endpoint = reqwest::Url::parse(&text(settings, "endpoint")?.to_utf8()?)
        .map_err(|_| IvyError::new("invalid_arguments", "Speech endpoint is invalid."))?;
    require(
        endpoint.scheme() == "https"
            && endpoint.username().is_empty()
            && endpoint.password().is_none()
            && endpoint.query().is_none()
            && endpoint.fragment().is_none(),
        "Speech endpoint requires HTTPS without embedded credentials.",
    )?;
    Ok(endpoint)
}
pub async fn synthesize_announcement(
    journal: &PhoneJournal,
    call: &IvyJson,
    announcement: JsString,
    settings: &IvyJson,
) -> IvyResult<IvyJson> {
    let endpoint = speech_endpoint(settings, &announcement)?;
    let hash = hash_json(&IvyJson::object([
        ("text", announcement.clone().into()),
        ("settings", settings.clone()),
    ]))?;
    let call_id = text(call, "callId")?;
    if let Some(prior) = journal
        .call_command(call_id.clone(), "call.screening.synthesize".into(), 0)
        .await?
    {
        require(
            prior.get("intent").and_then(|v| v.get("requestHash")) == Some(&hash.clone().into()),
            "Original speech request changed.",
        )
        .map_err(|_| IvyError::new("mutation_conflict", "Original speech request changed."))?;
        require(
            prior.get("phase") == Some(&"result".into())
                && prior.get("receipt").and_then(|v| v.get("ok")) == Some(&true.into()),
            "Original speech synthesis cannot be repeated.",
        )
        .map_err(|_| {
            IvyError::new(
                "phone_speech_unknown",
                "Original speech synthesis cannot be repeated.",
            )
        })?;
        return prior
            .get("receipt")
            .and_then(|v| v.get("result"))
            .cloned()
            .ok_or_else(unknown);
    }
    let credentials = phone_configuration_section(
        text(settings, "credentialsPath")?,
        PhoneConfigurationSection::Speech,
    )
    .await?;
    let key = text(&credentials, "apiKey")?.to_utf8()?;
    require(
        !key.is_empty() && key.len() <= 4096 && !key.contains(['\r', '\n']),
        "Protected speech API credential required.",
    )?;
    let operation = uuid::Uuid::new_v4().to_string();
    let intent = IvyJson::object([
        ("epoch", field(call, "epoch")?.clone()),
        ("callId", call_id.into()),
        ("operationId", operation.clone().into()),
        ("method", "call.screening.synthesize".into()),
        ("requestHash", hash.into()),
    ]);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|_| IvyError::internal())?;
    journal.submit(intent.clone()).await?;
    let result = async {
        let mut body = IvyJson::object([
            ("model", field(settings, "model")?.clone()),
            ("voice", field(settings, "voice")?.clone()),
            ("input", announcement.into()),
            ("response_format", "wav".into()),
            (
                "speed",
                settings.get("speed").cloned().unwrap_or(1.0.into()),
            ),
        ]);
        if let Some(instructions) = settings
            .get("instructions")
            .and_then(IvyJson::as_js_string)
            .filter(|v| !v.0.is_empty())
        {
            body.insert("instructions", instructions.clone().into());
        }
        let mut response = client
            .post(endpoint)
            .bearer_auth(key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(ivy_contracts::json::stringify(&body, 65536)?)
            .send()
            .await
            .map_err(|_| unknown())?;
        if !response.status().is_success() {
            return Err(unknown());
        }
        let mut wav = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| unknown())? {
            if wav.len() + chunk.len() > 10 * 1024 * 1024 {
                return Err(unknown());
            }
            wav.extend_from_slice(&chunk);
        }
        if wav.len() < 44 || &wav[..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
            return Err(unknown());
        }
        let root = journal.data_root().join("announcements");
        let file = root.join(format!("{operation}.wav"));
        let sha = digest(&wav);
        let target = file.clone();
        tokio::task::spawn_blocking(move || write_wav(&root, &target, &wav))
            .await
            .map_err(|_| unknown())??;
        Ok(IvyJson::object([
            ("wavPath", node_path::from_native(&file).into()),
            ("sha256", sha.into()),
        ]))
    }
    .await;
    let (ok, value, error) = match &result {
        Ok(value) => (true, value.clone(), IvyJson::Null),
        Err(_) => (false, IvyJson::Null, "operation_outcome_unknown".into()),
    };
    journal
        .finish(
            intent,
            IvyJson::object([
                ("version", 1.0.into()),
                ("epoch", field(call, "epoch")?.clone()),
                ("requestId", 1.0.into()),
                ("ok", ok.into()),
                ("result", value),
                ("error", error),
            ]),
        )
        .await?;
    result.map_err(|_| unknown())
}
fn write_wav(root: &Path, file: &PathBuf, wav: &[u8]) -> IvyResult<()> {
    let mut directories = std::fs::DirBuilder::new();
    directories.recursive(true);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        directories.mode(0o700);
        options.mode(0o600);
    }
    directories.create(root).map_err(|_| unknown())?;
    options
        .open(file)
        .and_then(|mut file| file.write_all(wav))
        .map_err(|_| unknown())
}
