use crate::{IvyError, IvyResult};
pub use crate::date_parse::{parse_date, parse_date_in_zone, parse_with_local_offset};
pub use crate::date_format::date_to_iso;
use chrono::{Datelike, TimeZone as _, Timelike, Utc};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

pub trait Clock: Send + Sync {
    fn utc_millis(&self) -> i64;
    fn monotonic(&self) -> Instant;
}
pub struct SystemClock;
impl Clock for SystemClock {
    fn utc_millis(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_millis() as i64,
            Err(e) => -(e.duration().as_millis() as i64),
        }
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalParts {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub weekday: u32,
}
pub struct TimeZone;
impl TimeZone {
    pub fn database_version() -> Option<&'static str> {
        jiff_tzdb::VERSION
    }
    pub fn local_parts(utc_millis: i64, zone: &str) -> IvyResult<LocalParts> {
        let utc = Utc
            .timestamp_millis_opt(utc_millis)
            .single()
            .ok_or_else(|| IvyError::new("invalid_arguments", "Invalid UTC date."))?;
        let local = if zone.starts_with(['+', '-']) {
            let bytes = zone.as_bytes();
            let digits = match bytes.len() {
                3 => Some((&bytes[1..3], &b"00"[..])),
                5 => Some((&bytes[1..3], &bytes[3..5])),
                6 if bytes[3] == b':' => Some((&bytes[1..3], &bytes[4..6])),
                _ => None,
            };
            let (hour, minute) = digits
                .filter(|(hour, minute)| hour.iter().chain(minute.iter()).all(u8::is_ascii_digit))
                .map(|(hour, minute)| {
                    (
                        (hour[0] - b'0') as i32 * 10 + (hour[1] - b'0') as i32,
                        (minute[0] - b'0') as i32 * 10 + (minute[1] - b'0') as i32,
                    )
                })
                .filter(|(hour, minute)| *hour < 24 && *minute < 60)
                .ok_or_else(|| IvyError::new("invalid_arguments", "Invalid time zone offset."))?;
            let offset = chrono::FixedOffset::east_opt(
                (hour * 3600 + minute * 60) * if bytes[0] == b'-' { -1 } else { 1 },
            )
            .unwrap();
            utc.with_timezone(&offset).naive_local()
        } else {
            let (name, data) = jiff_tzdb::get(zone)
                .ok_or_else(|| IvyError::new("invalid_arguments", "Invalid IANA time zone."))?;
            static ZONES: OnceLock<Mutex<HashMap<&'static str, Arc<tz::TimeZone>>>> =
                OnceLock::new();
            let zones = ZONES.get_or_init(|| Mutex::new(HashMap::new()));
            let zone = {
                let mut zones = zones.lock().unwrap();
                if let Some(zone) = zones.get(name) {
                    zone.clone()
                } else {
                    let zone = Arc::new(
                        tz::TimeZone::from_tz_data(data).map_err(|_| IvyError::internal())?,
                    );
                    zones.insert(name, zone.clone());
                    zone
                }
            };
            let seconds = zone
                .find_local_time_type(utc_millis.div_euclid(1000))
                .map_err(|_| IvyError::new("invalid_arguments", "Invalid UTC date."))?
                .ut_offset();
            let offset = chrono::FixedOffset::east_opt(seconds).ok_or_else(IvyError::internal)?;
            utc.with_timezone(&offset).naive_local()
        };
        Ok(LocalParts {
            year: local.year(),
            month: local.month(),
            day: local.day(),
            hour: local.hour(),
            minute: local.minute(),
            second: local.second(),
            weekday: local.weekday().num_days_from_sunday(),
        })
    }
}
pub struct ContactWindow {
    pub time_zone: String,
    pub start: String,
    pub end: String,
}
pub fn contact_window_open(window: Option<&ContactWindow>, utc_millis: i64) -> IvyResult<bool> {
    let Some(window) = window else {
        return Ok(true);
    };
    let parts = TimeZone::local_parts(utc_millis, &window.time_zone)?;
    let at = format!("{:02}:{:02}", parts.hour, parts.minute);
    Ok(if window.start < window.end {
        at >= window.start && at < window.end
    } else {
        at >= window.start || at < window.end
    })
}

pub async fn automatic_notifications_allowed(
    client: &dyn crate::rpc::RpcClient,
    utc_millis: i64,
    options: crate::rpc::RequestOptions,
) -> IvyResult<bool> {
    let result = client
        .request("settings.get", crate::IvyJson::Object(Vec::new()), options)
        .await?;
    let settings = result.required("settings")?;
    let Some(window) = settings.get("quietHours").filter(|v| !v.is_null()) else {
        return Ok(true);
    };
    Ok(!contact_window_open(
        Some(&ContactWindow {
            time_zone: window.required("timeZone")?.as_str()?,
            start: window.required("start")?.as_str()?,
            end: window.required("end")?.as_str()?,
        }),
        utc_millis,
    )?)
}
