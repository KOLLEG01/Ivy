//! Native date, Unicode and time-zone services used by the Hive executable.
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use ivy_contracts::{IvyError, IvyResult, JsString};
use std::{
    cmp::Ordering,
    rc::Rc,
    sync::{Arc, OnceLock},
};

#[derive(Clone, Copy)]
pub struct NativeRuntime;

fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
pub fn now() -> f64 {
    Utc::now().timestamp_millis() as f64
}
pub fn parse_date(value: &JsString) -> f64 {
    let value = text(value);
    DateTime::parse_from_rfc3339(&value)
        .map(|d| d.timestamp_millis())
        .or_else(|_| DateTime::parse_from_rfc2822(&value).map(|d| d.timestamp_millis()))
        .or_else(|_| {
            NaiveDate::parse_from_str(&value, "%Y-%m-%d")
                .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis())
        })
        .map(|n| n as f64)
        .unwrap_or(f64::NAN)
}
pub fn iso_date(milliseconds: f64) -> IvyResult<JsString> {
    if !milliseconds.is_finite() {
        return Err(IvyError::new("invalid_arguments", "Invalid date."));
    }
    DateTime::<Utc>::from_timestamp_millis(milliseconds as i64)
        .map(|d| d.to_rfc3339_opts(SecondsFormat::Millis, true).into())
        .ok_or_else(|| IvyError::new("invalid_arguments", "Invalid date."))
}
fn compare(a: &JsString, b: &JsString, english: bool) -> Ordering {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Globalization::CompareStringEx;
        let en: Vec<u16> = "en-US\0".encode_utf16().collect();
        let locale = if english {
            en.as_ptr()
        } else {
            std::ptr::null()
        };
        // NLS reads these immutable buffers only during this call.
        let value = unsafe {
            CompareStringEx(
                locale,
                0,
                a.0.as_ptr(),
                a.0.len() as i32,
                b.0.as_ptr(),
                b.0.len() as i32,
                std::ptr::null(),
                std::ptr::null(),
                0,
            )
        };
        match value {
            1 => return Ordering::Less,
            2 => return Ordering::Equal,
            3 => return Ordering::Greater,
            _ => {}
        }
    }
    let _ = english;
    text(a)
        .to_lowercase()
        .cmp(&text(b).to_lowercase())
        .then_with(|| a.0.cmp(&b.0))
}
impl crate::retention::Runtime for NativeRuntime {
    fn parse_date(&self, value: &JsString) -> f64 {
        parse_date(value)
    }
    fn iso_date(&self, value: f64) -> IvyResult<JsString> {
        iso_date(value)
    }
    fn compare_contract_keys(&self, a: &JsString, b: &JsString) -> Ordering {
        compare(a, b, false)
    }
}
impl crate::discovery::Runtime for NativeRuntime {
    fn lowercase(&self, value: &JsString) -> IvyResult<JsString> {
        Ok(text(value).to_lowercase().into())
    }
    fn word_character(&self, value: u32) -> bool {
        static WORD: OnceLock<regex::Regex> = OnceLock::new();
        char::from_u32(value).is_some_and(|c| {
            WORD.get_or_init(|| regex::Regex::new(r"^[\p{L}\p{N}]$").unwrap())
                .is_match(&c.to_string())
        })
    }
    fn compare_english(&self, a: &JsString, b: &JsString) -> Ordering {
        compare(a, b, true)
    }
}
impl crate::mcp::Runtime for NativeRuntime {
    fn uppercase(&self, value: &JsString) -> IvyResult<JsString> {
        Ok(text(value).to_uppercase().into())
    }
}
impl crate::store::QueryRuntime for NativeRuntime {
    fn parse_date(&self, value: &JsString) -> f64 {
        parse_date(value)
    }
    fn locale_lowercase(&self, value: &JsString) -> IvyResult<JsString> {
        Ok(text(value).to_lowercase().into())
    }
}
impl crate::hive_settings::TimeZones for NativeRuntime {
    fn supported(&self, value: &JsString) -> bool {
        let value = text(value);
        value.parse::<chrono_tz::Tz>().is_ok() || value.parse::<chrono::FixedOffset>().is_ok()
    }
}
pub fn startup_ports() -> crate::kernel::StartupPorts {
    crate::kernel::StartupPorts {
        runtime: Rc::new(NativeRuntime),
        query: Arc::new(NativeRuntime),
        clock: Rc::new(now),
        time_zones: Rc::new(NativeRuntime),
        temporary_directory: Box::new(|| {
            Ok(ivy_host_runtime::node_path::from_native(
                &std::env::temp_dir(),
            ))
        }),
    }
}
