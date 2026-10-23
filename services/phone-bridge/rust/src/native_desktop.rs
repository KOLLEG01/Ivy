//! Desktop command layer for NativeSipCommands' remaining runtime. SIP owns
//! its immediate stopping fence; the remaining platform owner retains cleanup.
use crate::{
    desktop::{
        DesktopActionError, PhoneDesktopRuntime, validate_application, validate_capture_text,
        validate_labels,
    },
    media::AudioDesktopIdentity,
    native_pipe::{NativeRuntime, NativeShutdown, NativeShutdownError},
    native_rpc::{
        NativeHandler, NativeInvocation, NativeInvocationStart, NativeOutcome, NativeRequest,
    },
    native_sip::{NativeSipOperation, NativeSipOperations, retained},
    sip::SipHostError,
};
use ivy_contracts::{
    IvyJson,
    json::{JsString, stringify},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

pub struct NativeDesktopCommands {
    desktop: PhoneDesktopRuntime,
    sip: NativeSipOperations,
    closed: Arc<AtomicBool>,
    remaining: Arc<dyn NativeRuntime>,
    shutdown: Mutex<Option<NativeSipOperation<()>>>,
}
impl NativeDesktopCommands {
    #[cfg(all(windows, target_pointer_width = "64"))]
    pub fn new(sip: NativeSipOperations, remaining: Arc<dyn NativeRuntime>) -> Self {
        Self::with_runtime(sip, PhoneDesktopRuntime::windows(), remaining)
    }
    pub fn with_runtime(
        sip: NativeSipOperations,
        desktop: PhoneDesktopRuntime,
        remaining: Arc<dyn NativeRuntime>,
    ) -> Self {
        Self {
            desktop,
            sip,
            remaining,
            closed: Arc::new(AtomicBool::new(false)),
            shutdown: Mutex::new(None),
        }
    }
    fn stopping(&self) -> bool {
        self.closed.load(Ordering::Acquire) || self.sip.is_stopping()
    }
    fn application(&self, request: NativeRequest) -> Result<NativeInvocation, &'static str> {
        let params = request.params();
        let (id, start) = if request.method() == "desktop.launch" {
            // Original DTO conversion precedes the closed check on launch.
            exact(params, &["appUserModelId", "startIfMissing"])?;
            let id = nullable_string(params.get("appUserModelId").unwrap(), "invalid_request")?;
            let start = params
                .get("startIfMissing")
                .unwrap()
                .as_bool()
                .ok_or("invalid_request")?;
            if self.stopping() {
                return Err("runtime_stopping");
            }
            (id, Some(start))
        } else {
            // Read-only commands check closed before decoding method fields.
            if self.stopping() {
                return Err("runtime_stopping");
            }
            exact(params, &["appUserModelId"])?;
            let id = nullable_string(
                params.get("appUserModelId").unwrap(),
                "operation_outcome_unknown",
            )?;
            validate_application(id.as_deref().ok_or("invalid_request")?).map_err(error_code)?;
            (id, None)
        };
        let desktop = self.desktop.clone();
        let sip = self.sip.clone();
        let closed = self.closed.clone();
        // Task.Run starts under native admission. The returned observer cannot
        // cancel a queued/entered platform action by being unpolled or dropped.
        let work = tokio::task::spawn_blocking(move || {
            let value = match start {
                Some(start) => desktop
                    .launch(id.as_deref(), start, &|| {
                        Ok(!closed.load(Ordering::Acquire) && !sip.is_stopping())
                    })
                    .map(|v| v.to_json()),
                None => desktop.observe(id.as_deref().unwrap()).map(|v| v.to_json()),
            };
            match value {
                Ok(value) => match native_result_json(&value) {
                    Ok(json) => NativeOutcome::result(json),
                    Err(_) => NativeOutcome::error("operation_outcome_unknown"),
                },
                Err(error) => NativeOutcome::error(error_code(error)),
            }
        });
        Ok(Box::pin(async move {
            work.await
                .unwrap_or_else(|_| NativeOutcome::error("operation_outcome_unknown"))
        }))
    }
    fn controls(&self, request: NativeRequest) -> Result<NativeInvocation, &'static str> {
        if self.stopping() {
            return Err("runtime_stopping");
        }
        let params = request.params();
        exact(params, &["desktop", "labels"])?;
        // Read<NativeDesktopIdentity> runs before string[] deserialization and
        // synchronous label validation. Preserve its raw Int32 token spelling.
        let identity = desktop_identity(
            params.get("desktop").unwrap(),
            raw_field(params, request.raw_params(), "desktop"),
        )?;
        let labels: Vec<JsString> = params
            .get("labels")
            .unwrap()
            .as_array()
            .ok_or("invalid_request")?
            .iter()
            .map(|value| {
                nullable_string(value, "invalid_request")?
                    .map(Into::into)
                    .ok_or("invalid_request")
            })
            .collect::<Result<_, _>>()?;
        validate_labels(&labels).map_err(|_| "invalid_request")?;
        let desktop = self.desktop.clone();
        let work = tokio::task::spawn_blocking(move || {
            // Like the original Task.Run, an admitted read can finish after
            // close. It neither grants call authority nor performs an Invoke.
            match desktop.controls(&identity, &labels) {
                Ok(value) => {
                    match native_result_json(&value.map_or(IvyJson::Null, |v| v.to_json())) {
                        Ok(json) => NativeOutcome::result(json),
                        Err(_) => NativeOutcome::error("operation_outcome_unknown"),
                    }
                }
                Err(error) => NativeOutcome::error(error_code(error)),
            }
        });
        Ok(Box::pin(async move {
            work.await
                .unwrap_or_else(|_| NativeOutcome::error("operation_outcome_unknown"))
        }))
    }
    fn capture(&self, request: NativeRequest) -> Result<NativeInvocation, &'static str> {
        if self.stopping() {
            return Err("runtime_stopping");
        }
        exact(request.params(), &["endpointId"])?;
        let id: JsString = nullable_string(
            request.params().get("endpointId").unwrap(),
            "operation_outcome_unknown",
        )?
        .ok_or("invalid_request")?
        .into();
        validate_capture_text(&id).map_err(error_code)?;
        let desktop = self.desktop.clone();
        let work = tokio::task::spawn_blocking(move || match desktop.capture(&id) {
            Ok(value) => native_result_json(&value.to_json())
                .map(NativeOutcome::result)
                .unwrap_or_else(|_| NativeOutcome::error("operation_outcome_unknown")),
            Err(error) => NativeOutcome::error(error_code(error)),
        });
        Ok(Box::pin(async move {
            work.await
                .unwrap_or_else(|_| NativeOutcome::error("operation_outcome_unknown"))
        }))
    }
    fn ownership(&self, request: NativeRequest) -> Result<NativeInvocation, &'static str> {
        if self.stopping() {
            return Err("runtime_stopping");
        }
        let process = request.method() == "desktop.process";
        let params = request.params();
        exact(
            params,
            if process {
                &["desktop", "pid"]
            } else {
                &["desktop", "endpointId"]
            },
        )?;
        let identity = desktop_identity(
            params.get("desktop").unwrap(),
            raw_field(params, request.raw_params(), "desktop"),
        )?;
        enum Query {
            Process(i32),
            CaptureOwner(JsString),
        }
        let query = if process {
            // JsonElement.GetInt32, unlike DTO deserialization, fails
            // generically on non-integer token spelling, wrong kind or range.
            let pid = raw_field(params, request.raw_params(), "pid")
                .parse::<i32>()
                .map_err(|_| "operation_outcome_unknown")?;
            if pid <= 0 {
                return Err("invalid_request");
            }
            Query::Process(pid)
        } else {
            let endpoint: JsString = nullable_string(
                params.get("endpointId").unwrap(),
                "operation_outcome_unknown",
            )?
            .ok_or("invalid_request")?
            .into();
            validate_capture_text(&endpoint).map_err(error_code)?;
            Query::CaptureOwner(endpoint)
        };
        let desktop = self.desktop.clone();
        let work = tokio::task::spawn_blocking(move || {
            let value = (|| {
                Ok::<_, DesktopActionError>(match query {
                    Query::Process(pid) => desktop
                        .process(&identity, pid)?
                        .ok_or(DesktopActionError::Unavailable)?
                        .to_json(),
                    Query::CaptureOwner(endpoint) => desktop
                        .capture_owner(&identity, &endpoint)?
                        .map_or(IvyJson::Null, |v| v.to_json()),
                })
            })();
            match value {
                Ok(value) => native_result_json(&value)
                    .map(NativeOutcome::result)
                    .unwrap_or_else(|_| NativeOutcome::error("operation_outcome_unknown")),
                Err(error) => NativeOutcome::error(error_code(error)),
            }
        });
        Ok(Box::pin(async move {
            work.await
                .unwrap_or_else(|_| NativeOutcome::error("operation_outcome_unknown"))
        }))
    }
}
pub(crate) fn raw_field<'a>(value: &IvyJson, raw: &'a str, name: &str) -> &'a str {
    let index = value
        .as_object()
        .unwrap()
        .iter()
        .position(|(key, _)| key == &JsString::from(name))
        .unwrap();
    crate::native_rpc::root_values(raw)[index]
}
pub(crate) fn desktop_identity(
    value: &IvyJson,
    raw: &str,
) -> Result<AudioDesktopIdentity, &'static str> {
    // EnumerateObject fails generically on a non-object, before exact fields.
    if value.as_object().is_none() {
        return Err("operation_outcome_unknown");
    }
    exact(
        value,
        &["pid", "startTimeUtcTicks", "imagePath", "appUserModelId"],
    )?;
    let pid = raw_field(value, raw, "pid")
        .parse::<i32>()
        .map_err(|_| "invalid_request")?;
    let ticks = nullable_string(value.get("startTimeUtcTicks").unwrap(), "invalid_request")?
        .ok_or("invalid_request")?;
    let image = nullable_string(value.get("imagePath").unwrap(), "invalid_request")?
        .ok_or("invalid_request")?;
    let application = nullable_string(value.get("appUserModelId").unwrap(), "invalid_request")?
        .ok_or("invalid_request")?;
    if ticks.is_empty()
        || ticks.len() > 19
        || ticks.starts_with('0')
        || !ticks.bytes().all(|c| c.is_ascii_digit())
        || image.encode_utf16().count() > 32767
    {
        return Err("invalid_request");
    }
    AudioDesktopIdentity::new(
        pid,
        ticks.parse::<i64>().map_err(|_| "invalid_request")?,
        std::path::Path::new(&image),
        &application,
    )
    .map_err(|_| "invalid_request")
}
// NativeRpc uses the original default BasicLatin JSON encoder. In particular,
// the response byte limit applies after that escaping, not UTF8/JS escaping.
// Adapt the shared writer's already-valid JSON; retain its structural/numeric
// behavior and transform only string contents, including replacement of lone
// UTF16 surrogates as in the original native serializer.
pub(crate) fn native_result_json(value: &IvyJson) -> Result<String, ()> {
    use std::fmt::Write;
    let raw = stringify(value, crate::ipc::FRAME_BYTES).map_err(|_| ())?;
    let mut chars = raw.chars();
    let mut quoted = false;
    let mut out = String::with_capacity(raw.len());
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                out.push(c);
            }
            '\\' if quoted => match chars.next().ok_or(())? {
                '"' => out.push_str("\\u0022"),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let unit = u16::from_str_radix(&hex, 16).map_err(|_| ())?;
                    write!(
                        out,
                        "\\u{:04X}",
                        if (0xd800..=0xdfff).contains(&unit) {
                            0xfffd
                        } else {
                            unit
                        }
                    )
                    .unwrap();
                }
                escape => {
                    out.push('\\');
                    out.push(escape);
                }
            },
            c if quoted && (c >= '\u{7f}' || "&'<>+`".contains(c)) => {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    write!(out, "\\u{unit:04X}").unwrap();
                }
            }
            _ => out.push(c),
        }
        if out.len() > crate::ipc::FRAME_BYTES - 512 {
            return Err(());
        }
    }
    Ok(out)
}
pub(crate) fn exact(params: &IvyJson, names: &[&str]) -> Result<(), &'static str> {
    if params
        .as_object()
        .is_none_or(|fields| fields.len() != names.len())
        || names.iter().any(|n| params.get(n).is_none())
    {
        Err("invalid_request")
    } else {
        Ok(())
    }
}
pub(crate) fn nullable_string(
    value: &IvyJson,
    wrong_kind: &'static str,
) -> Result<Option<String>, &'static str> {
    if value.is_null() {
        Ok(None)
    } else {
        let IvyJson::String(text) = value else {
            return Err(wrong_kind);
        };
        text.to_utf8().map(Some).map_err(|_| wrong_kind)
    }
}
fn error_code(error: DesktopActionError) -> &'static str {
    match error {
        DesktopActionError::Argument(_) => "invalid_request",
        _ => "operation_outcome_unknown",
    }
}
fn handles(method: &str) -> bool {
    matches!(
        method,
        "desktop.observe"
            | "desktop.launch"
            | "desktop.controls"
            | "desktop.capture"
            | "desktop.process"
            | "desktop.captureOwner"
    )
}
impl NativeHandler for NativeDesktopCommands {
    fn invoke(&self, request: NativeRequest) -> NativeInvocation {
        if !handles(request.method()) {
            return self.remaining.invoke(request);
        }
        let work = match request.method() {
            "desktop.controls" => self.controls(request),
            "desktop.capture" => self.capture(request),
            "desktop.process" | "desktop.captureOwner" => self.ownership(request),
            _ => self.application(request),
        };
        work.unwrap_or_else(|code| Box::pin(async move { NativeOutcome::error(code) }))
    }
    fn start(&self, request: NativeRequest) -> NativeInvocationStart {
        if !handles(request.method()) {
            return self.remaining.start(request);
        }
        let work = self.invoke(request);
        Box::pin(async move { work })
    }
    fn cleanup_slots(&self) -> usize {
        self.remaining.cleanup_slots()
    }
}
impl NativeRuntime for NativeDesktopCommands {
    fn shutdown(&self) -> NativeShutdown {
        self.closed.store(true, Ordering::Release);
        let mut saved = self.shutdown.lock().unwrap();
        let original = saved
            .get_or_insert_with(|| {
                let remaining = self.remaining.clone();
                retained(async move {
                    remaining
                        .shutdown()
                        .await
                        .map_err(|_| SipHostError::CleanupUnconfirmed)
                })
            })
            .clone();
        Box::pin(async move {
            original
                .wait()
                .await
                .map_err(|_| NativeShutdownError::OutcomeUnknown)
        })
    }
}

#[cfg(all(test, windows, target_pointer_width = "64"))]
#[path = "../../../../tests/rust/phone_native_desktop.rs"]
mod tests;
