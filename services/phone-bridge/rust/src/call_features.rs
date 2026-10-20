//! Original call-local access, screening and semantic keypad policy. Protected
//! codes and digits never appear in observations or native command receipts.
use crate::{
    dtmf::{InputDeduplicator, InputSource},
    dtmf_access::{AccessCodeGate, CodeProgress, FailureLimiter},
};
use ivy_contracts::IvyJson;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureError {
    InvalidRequest,
    OutcomeUnknown,
    AccessLimited,
}
impl FeatureError {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::OutcomeUnknown => "operation_outcome_unknown",
            Self::AccessLimited => "phone_access_limited",
        }
    }
}
type Result<T> = std::result::Result<T, FeatureError>;

pub struct CallAccessSettings {
    code: String,
    pub timeout_seconds: i32,
    pub max_failed_attempts: i32,
    pub failed_attempt_window_seconds: i32,
    pub failure_response_delay_milliseconds: i32,
}
impl std::fmt::Debug for CallAccessSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CallAccessSettings { protected code }")
    }
}
impl CallAccessSettings {
    pub fn new(code: String) -> Self {
        Self {
            code,
            timeout_seconds: 60,
            max_failed_attempts: 5,
            failed_attempt_window_seconds: 600,
            failure_response_delay_milliseconds: 1000,
        }
    }
    fn validate(&self) -> Result<()> {
        if !(3..=64).contains(&self.code.encode_utf16().count())
            || !(1..=120).contains(&self.timeout_seconds)
            || !(1..=100).contains(&self.max_failed_attempts)
            || !(1..=86_400).contains(&self.failed_attempt_window_seconds)
            || !(0..=5000).contains(&self.failure_response_delay_milliseconds)
            || AccessCodeGate::new(&self.code).is_none()
        {
            return Err(FeatureError::InvalidRequest);
        }
        Ok(())
    }
    /// Deserialize before call policy validation, as System.Text.Json does.
    /// Raw integer tokens reject fractional/exponent spellings and overflow.
    pub(crate) fn parse(value: &IvyJson, raw: &str) -> Result<Option<Self>> {
        if value.is_null() {
            return Ok(None);
        }
        let fields = value.as_object().ok_or(FeatureError::InvalidRequest)?;
        let tokens = crate::native_rpc::root_values(raw);
        let mut result = Self::new(String::new());
        for ((key, value), raw) in fields.iter().zip(tokens) {
            let key = key.to_utf8().map_err(|_| FeatureError::InvalidRequest)?;
            if key == "code" {
                result.code = if value.is_null() {
                    String::new()
                } else {
                    value.as_str().map_err(|_| FeatureError::InvalidRequest)?
                };
                continue;
            }
            let target = match key.as_str() {
                "timeoutSeconds" => &mut result.timeout_seconds,
                "maxFailedAttempts" => &mut result.max_failed_attempts,
                "failedAttemptWindowSeconds" => &mut result.failed_attempt_window_seconds,
                "failureResponseDelayMilliseconds" => {
                    &mut result.failure_response_delay_milliseconds
                }
                _ => return Err(FeatureError::InvalidRequest),
            };
            if !matches!(value, IvyJson::Number(_)) || raw.contains(['.', 'e', 'E']) {
                return Err(FeatureError::InvalidRequest);
            }
            *target = raw
                .parse::<i32>()
                .map_err(|_| FeatureError::InvalidRequest)?;
        }
        Ok(Some(result))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallFeatureCommand {
    pub command: &'static str,
    pub sequence: i64,
    pub model: Option<&'static str>,
    pub reasoning_effort: Option<&'static str>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallFeatureObservation {
    pub access: &'static str,
    pub disable_codec_upgrade: bool,
    pub screening: &'static str,
    pub command: Option<&'static str>,
    pub command_sequence: i64,
    pub command_state: Option<&'static str>,
    pub model: Option<&'static str>,
    pub reasoning_effort: Option<&'static str>,
    pub commands: Vec<CallFeatureCommand>,
    pub command_overflow: bool,
}
fn nullable(value: Option<&'static str>) -> String {
    value.map_or_else(|| "null".into(), |s| format!("\"{s}\""))
}
impl CallFeatureObservation {
    /// All strings are closed policy values. Preserve native Int64 lexemes,
    /// property order and nulls without a lossy ECMAScript-number round trip.
    pub fn json(&self) -> String {
        let commands = self
            .commands
            .iter()
            .map(|c| {
                format!(
                    "{{\"command\":\"{}\",\"sequence\":{},\"model\":{},\"reasoningEffort\":{}}}",
                    c.command,
                    c.sequence,
                    nullable(c.model),
                    nullable(c.reasoning_effort)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"access\":\"{}\",\"disableCodecUpgrade\":{},\"screening\":\"{}\",\"command\":{},\"commandSequence\":{},\"commandState\":{},\"model\":{},\"reasoningEffort\":{},\"commands\":[{}],\"commandOverflow\":{}}}",
            self.access,
            self.disable_codec_upgrade,
            self.screening,
            nullable(self.command),
            self.command_sequence,
            nullable(self.command_state),
            nullable(self.model),
            nullable(self.reasoning_effort),
            commands,
            self.command_overflow
        )
    }
}

pub type FeatureDelay = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
/// UTC failures and the challenge deadline follow the original injected time.
/// Rejection responses deliberately use system elapsed time instead.
pub trait CallFeatureTime: Send + Sync + 'static {
    fn utc_ticks(&self) -> i64;
    fn delay(&self, duration: Duration) -> FeatureDelay;
}
#[derive(Default)]
pub struct SystemCallFeatureTime;
impl CallFeatureTime for SystemCallFeatureTime {
    fn utc_ticks(&self) -> i64 {
        const EPOCH: i64 = 621_355_968_000_000_000;
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => EPOCH + (d.as_nanos() / 100) as i64,
            Err(e) => EPOCH - (e.duration().as_nanos() / 100) as i64,
        }
    }
    fn delay(&self, duration: Duration) -> FeatureDelay {
        let deadline = tokio::time::Instant::now() + duration;
        Box::pin(async move { tokio::time::sleep_until(deadline).await })
    }
}
struct State {
    gate: Option<AccessCodeGate>,
    settings: Option<CallAccessSettings>,
    access: &'static str,
    screening: &'static str,
    command: Option<&'static str>,
    model: Option<&'static str>,
    effort: Option<&'static str>,
    commands: Vec<CallFeatureCommand>,
    overflow: bool,
    sequence: i64,
    buffer: String,
    dedup: InputDeduplicator,
    configured: bool,
    connected: bool,
    closed: bool,
}
impl State {
    fn authenticated(&self) -> bool {
        matches!(self.access, "trusted" | "authenticated")
    }
    fn needs_inband(&self) -> bool {
        !self.closed
            && self.configured
            && (self.access == "awaiting_code"
                || self.screening == "waiting"
                || self.connected && self.authenticated() && self.screening == "none")
    }
    fn view(&self) -> CallFeatureObservation {
        CallFeatureObservation {
            access: self.access,
            disable_codec_upgrade: self
                .gate
                .as_ref()
                .is_some_and(AccessCodeGate::disable_codec_upgrade),
            screening: self.screening,
            command: self.command,
            command_sequence: self.sequence,
            command_state: self.command.map(|_| "pending"),
            model: self.model,
            reasoning_effort: self.effort,
            commands: self.commands.clone(),
            command_overflow: self.overflow,
        }
    }
    fn command_digit(&mut self, digit: char) {
        if digit == '*' {
            self.buffer = "*".into();
        } else if !self.buffer.is_empty() && self.buffer.encode_utf16().count() < 32 {
            self.buffer.push(digit);
        } else {
            self.buffer.clear();
        }
        if digit != '#' {
            return;
        }
        let mut next = None;
        let mut model = None;
        let mut effort = None;
        if self.buffer == "*0#" {
            next = Some("new_voice");
        } else {
            let b = self.buffer.as_bytes();
            if b.len() == 5 && b[0] == b'*' && b[1] == b'1' && b[4] == b'#' {
                model = match b[2] {
                    b'1' => Some("luna"),
                    b'2' => Some("sol"),
                    b'3' => Some("astra"),
                    _ => None,
                };
                effort = match b[3] {
                    b'1' => Some("low"),
                    b'2' => Some("medium"),
                    b'3' => Some("high"),
                    b'4' => Some("xhigh"),
                    b'5' => Some("max"),
                    b'6' => Some("ultra"),
                    _ => None,
                };
                if model.is_some()
                    && effort.is_some()
                    && !(model == Some("luna") && effort == Some("ultra"))
                {
                    next = Some("select_voice");
                }
            }
        }
        self.buffer.clear();
        if let Some(next) = next {
            self.command = Some(next);
            if next == "select_voice" {
                self.model = model;
                self.effort = effort;
            }
            self.sequence = self.sequence.wrapping_add(1);
            self.commands.push(CallFeatureCommand {
                command: next,
                sequence: self.sequence,
                model: self.model,
                reasoning_effort: self.effort,
            });
            if self.commands.len() > 32 {
                self.commands.remove(0);
                self.overflow = true;
            }
        }
    }
}
struct Core {
    state: Mutex<State>,
    limiter: Arc<Mutex<FailureLimiter>>,
    time: Arc<dyn CallFeatureTime>,
    hangup: Arc<dyn Fn() + Send + Sync>,
    stop: CancellationToken,
    runtime: Handle,
    input_origin: Instant,
}
impl Core {
    fn fail(&self, s: &mut State, access: &'static str) -> Result<()> {
        s.access = access;
        let settings = s.settings.as_ref().expect("original challenge");
        self.limiter
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record_failure(
                self.time.utc_ticks(),
                settings.max_failed_attempts,
                i64::from(settings.failed_attempt_window_seconds) * 10_000_000,
            )
            .map_err(|_| FeatureError::OutcomeUnknown)
    }
    fn dispose(&self) {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if s.closed {
            return;
        }
        s.closed = true;
        s.buffer.clear();
        self.stop.cancel();
    }
}
struct Owner(Arc<Core>);
impl Drop for Owner {
    fn drop(&mut self) {
        self.0.dispose();
    }
}
#[derive(Clone)]
pub struct PhoneCallFeatures(Arc<Owner>);
impl PhoneCallFeatures {
    pub fn new(
        limiter: Arc<Mutex<FailureLimiter>>,
        time: Arc<dyn CallFeatureTime>,
        hangup: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self> {
        let runtime = Handle::try_current().map_err(|_| FeatureError::OutcomeUnknown)?;
        Ok(Self(Arc::new(Owner(Arc::new(Core {
            state: Mutex::new(State {
                gate: None,
                settings: None,
                access: "trusted",
                screening: "none",
                command: None,
                model: None,
                effort: None,
                commands: Vec::new(),
                overflow: false,
                sequence: 0,
                buffer: String::new(),
                dedup: InputDeduplicator::default(),
                configured: false,
                connected: false,
                closed: false,
            }),
            limiter,
            time,
            hangup: Arc::new(hangup),
            stop: CancellationToken::new(),
            runtime,
            input_origin: Instant::now(),
        })))))
    }
    pub fn observation(&self) -> CallFeatureObservation {
        self.0
            .0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .view()
    }
    pub fn authenticated(&self) -> bool {
        self.0
            .0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .authenticated()
    }
    pub fn needs_inband_dtmf(&self) -> bool {
        self.0
            .0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .needs_inband()
    }
    pub fn configure(&self, settings: Option<CallAccessSettings>, incoming: bool) -> Result<()> {
        let c = &self.0.0;
        let mut s = c.state.lock().unwrap_or_else(|p| p.into_inner());
        if s.closed || s.configured {
            return Err(FeatureError::OutcomeUnknown);
        }
        if settings.is_some() && !incoming {
            return Err(FeatureError::InvalidRequest);
        }
        if let Some(settings) = &settings {
            settings.validate()?;
        }
        s.configured = true;
        let Some(settings) = settings else {
            return Ok(());
        };
        s.access = "invalid_code";
        s.settings = Some(settings);
        let settings = s.settings.as_ref().unwrap();
        let decision = c
            .limiter
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .check(
                c.time.utc_ticks(),
                settings.max_failed_attempts,
                i64::from(settings.failed_attempt_window_seconds) * 10_000_000,
            )
            .map_err(|_| FeatureError::OutcomeUnknown)?;
        if !decision.allowed {
            return Err(FeatureError::AccessLimited);
        }
        s.gate = AccessCodeGate::new(&settings.code);
        s.access = "awaiting_code";
        Ok(())
    }
    pub fn connected(&self) {
        let c = &self.0.0;
        let mut s = c.state.lock().unwrap_or_else(|p| p.into_inner());
        s.connected = true;
        if s.closed || s.access != "awaiting_code" {
            return;
        }
        // Construct the injected delay now, before publishing this transition.
        let deadline = c.time.delay(Duration::from_secs(
            s.settings.as_ref().unwrap().timeout_seconds as u64,
        ));
        let core = c.clone();
        c.runtime.spawn(async move {
            tokio::select! { biased;
                _ = core.stop.cancelled() => return,
                _ = deadline => {},
            }
            {
                let mut s = core.state.lock().unwrap_or_else(|p| p.into_inner());
                if s.closed || s.access != "awaiting_code" {
                    return;
                }
                if core.fail(&mut s, "timeout").is_err() {
                    return;
                }
            }
            (core.hangup)();
        });
    }
    pub fn digit(&self, digit: char, source: InputSource) -> Result<()> {
        let c = &self.0.0;
        let mut s = c.state.lock().unwrap_or_else(|p| p.into_inner());
        if s.closed || !s.configured || !s.dedup.is_new(digit, source, c.input_origin.elapsed()) {
            return Ok(());
        }
        let mut rejected = false;
        if s.access == "awaiting_code" {
            match s.gate.as_mut().unwrap().process(digit) {
                CodeProgress::Accepted => s.access = "authenticated",
                CodeProgress::Rejected => {
                    c.fail(&mut s, "invalid_code")?;
                    rejected = true;
                }
                _ => {}
            }
        } else if s.authenticated() {
            if s.screening == "waiting" {
                if digit == '1' {
                    s.screening = "accepted";
                } else if digit == '2' {
                    s.screening = "declined";
                    rejected = true;
                }
            } else if s.screening == "none" && s.connected {
                s.command_digit(digit);
            }
        }
        if rejected {
            let delay = s
                .settings
                .as_ref()
                .map_or(0, |v| v.failure_response_delay_milliseconds);
            let core = c.clone();
            c.runtime.spawn(async move {
                // Teardown never runs inline in an RTP/decoder callback.
                tokio::task::yield_now().await;
                tokio::select! { biased;
                    _ = core.stop.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_millis(delay as u64)) => {},
                }
                (core.hangup)();
            });
        }
        Ok(())
    }
    pub fn start_screening(&self) -> Result<()> {
        let mut s = self.0.0.state.lock().unwrap_or_else(|p| p.into_inner());
        if s.closed || !s.authenticated() || s.screening != "none" {
            return Err(FeatureError::OutcomeUnknown);
        }
        s.screening = "waiting";
        Ok(())
    }
    pub fn bridge_screening(&self) -> Result<()> {
        let mut s = self.0.0.state.lock().unwrap_or_else(|p| p.into_inner());
        if s.closed || s.screening != "accepted" {
            return Err(FeatureError::OutcomeUnknown);
        }
        s.screening = "bridged";
        Ok(())
    }
    pub fn screening_timeout(&self) {
        let c = &self.0.0;
        {
            let mut s = c.state.lock().unwrap_or_else(|p| p.into_inner());
            if s.closed || s.screening != "waiting" {
                return;
            }
            s.screening = "timeout";
        }
        (c.hangup)();
    }
    pub fn dispose(&self) {
        self.0.0.dispose();
    }
}
