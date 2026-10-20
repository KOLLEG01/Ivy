//! Retry only the original failed native turn. Date arithmetic and current
//! question policy are required owner bindings, never inferred from prose.
use crate::{
    native_inspector::{Inspection, InspectionStore, NativeInspector, PageSize},
    operations::{Document, Store},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, json::canonical, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::clock::LocalParts;

/// Bind exact ECMAScript Date/Intl behavior. UTC construction and ISO formatting
/// belong to the shared runtime; there is no service-local calendar fallback.
pub trait RetryDates {
    type Zone;
    fn parse(&self, value: &JsString) -> f64;
    fn zone(&self, name: &JsString) -> IvyResult<Self::Zone>;
    fn parts(&self, zone: &Self::Zone, at: f64) -> IvyResult<LocalParts>;
    fn utc(&self, year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> f64;
    fn iso(&self, at: f64) -> IvyResult<JsString>;
}
/// The owner supplies exact Date.parse and Intl behavior, including its local
/// time zone and locale. No process defaults are selected by the retry adapter.
pub trait RetryDateContext {
    type Zone;
    fn parse(&self, value: &JsString) -> f64;
    fn zone(&self, name: &JsString) -> IvyResult<Self::Zone>;
    fn parts(&self, zone: &Self::Zone, at: f64) -> IvyResult<LocalParts>;
}
pub struct SharedRetryDates<D> {
    pub context: D,
}
impl<D: RetryDateContext> RetryDates for SharedRetryDates<D> {
    type Zone = D::Zone;
    fn parse(&self, value: &JsString) -> f64 {
        self.context.parse(value)
    }
    fn zone(&self, name: &JsString) -> IvyResult<Self::Zone> {
        self.context.zone(name)
    }
    fn parts(&self, zone: &Self::Zone, at: f64) -> IvyResult<LocalParts> {
        self.context.parts(zone, at)
    }
    fn utc(&self, year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> f64 {
        ivy_sdk::clock::date_utc(
            year as f64,
            month as f64,
            day as f64,
            hour as f64,
            minute as f64,
            second as f64,
            0.0,
        )
    }
    fn iso(&self, at: f64) -> IvyResult<JsString> {
        // The source normalizes the thrown RangeError through IvyError.from.
        ivy_sdk::clock::date_to_iso(at).map_err(|_| IvyError::internal())
    }
}
pub trait RetryQuestions {
    fn unanswered(&self, task: &Json, run: &JsString) -> IvyResult<bool>;
}
#[allow(async_fn_in_trait)]
pub trait RetryInspection {
    async fn find(&self, run: &Json) -> IvyResult<Inspection>;
}
impl<S: InspectionStore> RetryInspection for NativeInspector<'_, S> {
    async fn find(&self, run: &Json) -> IvyResult<Inspection> {
        NativeInspector::find(self, run, 8.0, PageSize::Hundred).await
    }
}
fn field<'a>(v: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(key).unwrap_or(&NULL)
}
fn equal(v: &Json, key: &str, value: &str) -> bool {
    v.get(key) == Some(&Json::from(value))
}
fn truthy(v: &Json) -> bool {
    match v {
        Json::Null => false,
        Json::Bool(v) => *v,
        Json::Number(v) => *v != 0.0 && !v.is_nan(),
        Json::String(v) => !v.0.is_empty(),
        _ => true,
    }
}
fn same(a: Option<&Json>, b: Option<&Json>) -> IvyResult<bool> {
    Ok(match (a, b) {
        (Some(a), Some(b)) => {
            canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?
        }
        (None, None) => true,
        _ => false,
    })
}
fn ascii(unit: u16) -> u16 {
    if (65..=90).contains(&unit) {
        unit + 32
    } else {
        unit
    }
}
fn starts(units: &[u16], text: &str) -> bool {
    units.len() >= text.len()
        && units
            .iter()
            .zip(text.bytes())
            .all(|(a, b)| ascii(*a) == u16::from(b))
}
fn word(unit: u16) -> bool {
    matches!(unit, 48..=57 | 65..=90 | 97..=122 | 95)
}
fn boundary(units: &[u16], at: usize) -> bool {
    units.get(at.wrapping_sub(1)).is_some_and(|u| word(*u))
        != units.get(at).is_some_and(|u| word(*u))
}
fn space(unit: u16) -> bool {
    matches!(unit, 0x9..=0xd | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
fn spaces(units: &[u16], mut at: usize) -> usize {
    while units.get(at).is_some_and(|u| space(*u)) {
        at += 1;
    }
    at
}
fn digits(units: &[u16], at: &mut usize, count: usize) -> Option<u32> {
    let selected = units.get(*at..*at + count)?;
    if !selected.iter().all(|u| (48..=57).contains(u)) {
        return None;
    }
    *at += count;
    Some(selected.iter().fold(0, |n, u| n * 10 + u32::from(*u) - 48))
}
fn token(units: &[u16], at: &mut usize, value: u16) -> Option<()> {
    if units.get(*at) != Some(&value) {
        return None;
    }
    *at += 1;
    Some(())
}
fn iso_end(units: &[u16], mut at: usize) -> Option<usize> {
    digits(units, &mut at, 4)?;
    token(units, &mut at, 45)?;
    digits(units, &mut at, 2)?;
    token(units, &mut at, 45)?;
    digits(units, &mut at, 2)?;
    token(units, &mut at, 84).or_else(|| token(units, &mut at, 116))?;
    digits(units, &mut at, 2)?;
    token(units, &mut at, 58)?;
    digits(units, &mut at, 2)?;
    token(units, &mut at, 58)?;
    digits(units, &mut at, 2)?;
    if units.get(at) == Some(&46) {
        at += 1;
        let first = at;
        while units.get(at).is_some_and(|u| (48..=57).contains(u)) {
            at += 1;
        }
        if first == at {
            return None;
        }
    }
    if units.get(at).is_some_and(|u| ascii(*u) == 122) {
        return Some(at + 1);
    }
    if !units.get(at).is_some_and(|u| *u == 43 || *u == 45) {
        return None;
    }
    at += 1;
    digits(units, &mut at, 2)?;
    token(units, &mut at, 58)?;
    digits(units, &mut at, 2)?;
    Some(at)
}
/// Match the source's reset prefix, including its literal optional " at" and
/// ECMAScript whitespace/word boundaries, without a different regex dialect.
fn reset_starts(units: &[u16]) -> impl Iterator<Item = usize> + '_ {
    (0..units.len()).filter_map(|at| {
        if !boundary(units, at) || !starts(&units[at..], "reset") {
            return None;
        }
        let mut end = at + 5;
        if units.get(end).is_some_and(|u| ascii(*u) == 115) {
            end += 1;
        }
        if starts(&units[end..], " at") {
            end += 3;
        }
        let next = spaces(units, end);
        (next > end).then_some(next)
    })
}
fn has_phrase(units: &[u16], text: &str) -> bool {
    (0..units.len()).any(|at| {
        boundary(units, at) && starts(&units[at..], text) && boundary(units, at + text.len())
    })
}
fn usage_message(units: &[u16]) -> bool {
    for prefix in [
        "you've hit your session limit",
        "you've hit your weekly limit",
        "you've hit your usage limit",
        "rate limit reached",
        "rate limit exceeded",
    ] {
        if starts(units, prefix) && boundary(units, prefix.len()) {
            return true;
        }
    }
    if starts(units, "you") && units.get(3) == Some(&0x2019) {
        for suffix in [
            "ve hit your session limit",
            "ve hit your weekly limit",
            "ve hit your usage limit",
        ] {
            if starts(&units[4..], suffix) && boundary(units, 4 + suffix.len()) {
                return true;
            }
        }
    }
    if starts(units, "claude ") {
        // The one-to-64 ASCII word/space span is greedy, with ordinary regex
        // backtracking before the literal suffix and its trailing word boundary.
        for size in (1..=64).rev() {
            let end = 7 + size;
            if units
                .get(7..end)
                .is_some_and(|v| v.iter().all(|u| word(*u) || *u == 32))
                && units
                    .get(end..)
                    .is_some_and(|v| starts(v, " limit reached"))
                && boundary(units, end + 14)
            {
                return true;
            }
        }
    }
    false
}
fn local_reset<D: RetryDates>(dates: &D, message: &JsString, failed_at: f64) -> Option<f64> {
    let units = &message.0;
    for start in reset_starts(units) {
        if let Some(end) = iso_end(units, start) {
            let at = dates.parse(&JsString(units[start..end].to_vec()));
            return at.is_finite().then_some(at);
        }
    }
    for start in reset_starts(units) {
        let mut at = start;
        let Some(mut hour) = digits(units, &mut at, 1) else {
            continue;
        };
        if units.get(at).is_some_and(|u| (48..=57).contains(u)) {
            hour = hour * 10 + digits(units, &mut at, 1)?;
        }
        if token(units, &mut at, 58).is_none() {
            continue;
        }
        let Some(minute) = digits(units, &mut at, 2) else {
            continue;
        };
        at = spaces(units, at);
        let period = if starts(&units[at..], "am") {
            Some(false)
        } else if starts(&units[at..], "pm") {
            Some(true)
        } else {
            None
        };
        if period.is_some() {
            at += 2;
        }
        at = spaces(units, at);
        if token(units, &mut at, 40).is_none() {
            continue;
        }
        let Some(length) = units[at..].iter().position(|u| *u == 41) else {
            continue;
        };
        if length == 0 {
            continue;
        }
        if minute > 59 || period.map_or(hour > 23, |_| hour < 1 || hour > 12) {
            return None;
        }
        if let Some(pm) = period {
            hour = hour % 12 + if pm { 12 } else { 0 };
        }
        let zone = dates
            .zone(&JsString(units[at..at + length].to_vec()))
            .ok()?;
        let day = dates.parts(&zone, failed_at).ok()?;
        let resolve = |offset: u32| -> IvyResult<f64> {
            let target = dates.utc(day.year, day.month - 1, day.day + offset, hour, minute, 0);
            let mut at = target;
            for _ in 0..3 {
                let local = dates.parts(&zone, at)?;
                let actual = dates.utc(
                    local.year,
                    local.month - 1,
                    local.day,
                    local.hour,
                    local.minute,
                    local.second,
                );
                at += target - actual;
            }
            Ok(at)
        };
        let today = resolve(0).ok()?;
        let reset = if today + 60000.0 >= failed_at {
            today
        } else {
            resolve(1).ok()?
        };
        return if (has_phrase(units, "session limit") || has_phrase(units, "five hour"))
            && reset - failed_at > 18000000.0
        {
            None
        } else {
            Some(reset)
        };
    }
    None
}
pub fn waiting_detail(at: &JsString) -> JsString {
    JsString(
        "Agent temporarily unavailable; automatic continuation after "
            .encode_utf16()
            .chain(at.0.iter().copied())
            .chain([46])
            .collect(),
    )
}
pub fn native_rate_limit_retry_at<D: RetryDates>(
    dates: &D,
    turn: &Json,
    observed_at: &JsString,
) -> IvyResult<Option<JsString>> {
    if !equal(turn, "status", "failed") {
        return Ok(None);
    }
    let error = field(turn, "error");
    let message = field(error, "message")
        .as_js_string()
        .cloned()
        .unwrap_or_default();
    let observed = dates.parse(observed_at);
    let failed_at = turn
        .get("completedAt")
        .and_then(Json::as_f64)
        .filter(|v| v.is_finite() && *v * 1000.0 <= observed)
        .map_or(observed, |v| v * 1000.0);
    let capacity = "selected model is at capacity";
    if equal(error, "codexErrorInfo", "serverOverloaded")
        || starts(&message.0, capacity)
            && message
                .0
                .get(capacity.len())
                .is_none_or(|u| *u == 46 || *u == 33)
    {
        return dates.iso(failed_at + 60000.0).map(Some);
    }
    if !equal(error, "codexErrorInfo", "usageLimitExceeded") && !usage_message(&message.0) {
        return Ok(None);
    }
    let reset = local_reset(dates, &message, failed_at);
    let at = reset
        .filter(|at| *at >= failed_at)
        .map_or(failed_at + 900000.0, |at| at + 60000.0);
    // Math.max propagates NaN, unlike Rust f64::max.
    dates
        .iso(if failed_at.is_nan() || at.is_nan() {
            f64::NAN
        } else {
            (failed_at + 60000.0).max(at)
        })
        .map(Some)
}
pub struct FailedRateLimit<'a, S, Q, D, I> {
    pub store: &'a S,
    pub questions: &'a Q,
    pub dates: &'a D,
    pub inspector: &'a I,
}
impl<S: Store, Q: RetryQuestions, D: RetryDates, I: RetryInspection>
    FailedRateLimit<'_, S, Q, D, I>
{
    pub async fn retry_at(&self, task: &Document) -> IvyResult<Option<JsString>> {
        let value = &task.value;
        let primary = field(value, "primaryResourceRef");
        let last = field(value, "lastRun");
        if truthy(field(value, "blocker"))
            || !equal(field(value, "waiting"), "reason", "user")
            || !equal(
                field(value, "waiting"),
                "detail",
                "Execution failed; see the ticket comment.",
            )
            || !truthy(primary)
            || !equal(primary, "namespace", "codex")
            || !equal(primary, "kind", "thread")
            || !truthy(last)
        {
            return Ok(None);
        }
        if self.questions.unanswered(
            value,
            last.required("objectId")?
                .as_js_string()
                .ok_or_else(ivy_contracts::IvyError::internal)?,
        )? {
            return Ok(None);
        }
        let run = self.store.read("task-board/run", last, true).await?;
        let r = &run.value;
        if r.get("taskId") != task.pin.get("objectId")
            || !equal(r, "phase", "failed")
            || !equal(field(r, "externalOutcome"), "code", "native_failed")
            || !equal(field(r, "externalOutcome"), "state", "failed")
            || truthy(field(r, "cancellation"))
            || !truthy(field(r, "turnId"))
            || !truthy(field(r, "nativeEpoch"))
            || !truthy(field(r, "result"))
            || !same(r.get("result"), value.get("latestResult"))?
            || !same(r.get("primaryResourceRef"), Some(primary))?
        {
            return Ok(None);
        }
        let result = self
            .store
            .read("task-board/result", r.required("result")?, true)
            .await?;
        let content = field(&result.value, "content");
        let finished = field(r, "finishedAt")
            .as_js_string()
            .cloned()
            .unwrap_or_default();
        if !equal(content, "summary", "Native turn failed.")
            && native_rate_limit_retry_at(
                self.dates,
                &Json::object([
                    ("status", "failed".into()),
                    (
                        "error",
                        Json::object([("message", field(content, "summary").clone())]),
                    ),
                ]),
                &finished,
            )?
            .is_none()
        {
            return Ok(None);
        }
        let Inspection::Found { metadata, turns } = self.inspector.find(&run.pin).await? else {
            return Ok(None);
        };
        let m = field(&metadata, "reply");
        let t = field(&turns, "reply");
        if m.get("result").is_none()
            || t.get("result").is_none()
            || equal(
                field(field(field(m, "result"), "thread"), "status"),
                "type",
                "active",
            )
        {
            return Ok(None);
        }
        let turn = field(field(t, "result"), "data")
            .as_array()
            .unwrap_or(&[])
            .iter()
            .find(|turn| turn.get("id") == r.get("turnId"));
        match turn {
            None => Ok(None),
            Some(turn) => native_rate_limit_retry_at(
                self.dates,
                turn,
                if field(r, "finishedAt").is_null() {
                    field(&turns, "observedAt")
                        .as_js_string()
                        .unwrap_or(&finished)
                } else {
                    &finished
                },
            ),
        }
    }
}
