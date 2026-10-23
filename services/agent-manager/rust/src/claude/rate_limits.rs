//! Original durable subscription windows and per-process usage-refresh clock.
//! Derived from the pinned adapter; see services/agent-manager/claude/LICENSE.adapter.
use super::{
    error::Result,
    value::{self, field},
};
use ivy_contracts::{IvyJson, JsString};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub struct RateLimitStore {
    path: PathBuf,
    windows: BTreeMap<JsString, IvyJson>,
    usage_at: f64,
}
fn percent(n: f64) -> IvyJson {
    IvyJson::Number(value::minimum(100.0, value::maximum(0.0, value::round(n))))
}
fn seconds(value: &IvyJson) -> IvyJson {
    let n = match value {
        IvyJson::Number(v) if v.is_finite() => *v,
        IvyJson::String(v) => ivy_sdk::clock::parse_date(v) / 1000.0,
        _ => return IvyJson::Null,
    };
    if n.is_finite() {
        IvyJson::Number(value::round(n))
    } else {
        IvyJson::Null
    }
}
impl RateLimitStore {
    pub fn open(home: &Path) -> Self {
        let path = home.join("rate-limits.json");
        let windows = fs::read(&path)
            .ok()
            .and_then(|bytes| value::decode(&bytes).ok())
            .and_then(|value| {
                value
                    .as_object()
                    .map(|entries| entries.iter().cloned().collect())
            })
            .unwrap_or_default();
        Self {
            path,
            windows,
            usage_at: 0.0,
        }
    }
    pub fn usage_due(&self, now: f64, interval: f64) -> bool {
        now - self.usage_at >= interval
    }
    fn save(&mut self, next: BTreeMap<JsString, IvyJson>) {
        self.windows = next;
        let _ = value::save_json(&self.path, &IvyJson::object(self.windows.clone()));
    }
    fn copy_windows(&self) -> BTreeMap<JsString, IvyJson> {
        self.windows.clone()
    }
    pub fn record_event(&mut self, info: &IvyJson, now: f64) {
        let Some(kind) = field(info, "rateLimitType")
            .as_js_string()
            .filter(|k| !k.0.is_empty() && **k != JsString::from("overage"))
        else {
            return;
        };
        let Some(utilization) = field(info, "utilization").as_f64() else {
            return;
        };
        let mut window = IvyJson::object([
            ("usedPercent", percent(utilization * 100.0)),
            ("resetsAt", seconds(field(info, "resetsAt"))),
            ("observedAt", IvyJson::Number(now)),
        ]);
        if field(info, "status") == &IvyJson::from("rejected") {
            window.insert("rejected", true.into());
        }
        let mut next = self.copy_windows();
        next.insert(kind.clone(), window);
        self.save(next);
    }
    pub fn record_usage(&mut self, limits: &IvyJson, now: f64) {
        self.usage_at = now;
        if limits.as_object().is_none() {
            return;
        }
        let mut next = self.copy_windows();
        let mut add = |key: JsString, entry: &IvyJson| {
            if let Some(n) = field(entry, "utilization").as_f64() {
                next.insert(
                    key,
                    IvyJson::object([
                        ("usedPercent", percent(n)),
                        ("resetsAt", seconds(field(entry, "resets_at"))),
                        ("observedAt", IvyJson::Number(now)),
                    ]),
                );
            }
        };
        for key in [
            "five_hour",
            "seven_day",
            "seven_day_opus",
            "seven_day_sonnet",
            "seven_day_oauth_apps",
        ] {
            add(key.into(), field(limits, key));
        }
        if let Some(entries) = field(limits, "model_scoped").as_array() {
            for entry in entries {
                if let Some(name) = field(entry, "display_name")
                    .as_js_string()
                    .filter(|name| !name.0.is_empty())
                {
                    let mut key = JsString::from("model:");
                    key.0.extend_from_slice(&name.0);
                    add(key, entry);
                }
            }
        }
        self.save(next);
    }
    pub fn read(&self, now: f64) -> Result<IvyJson> {
        let window = |key: &JsString, minutes: f64| {
            let Some(v) = self.windows.get(key).filter(|v| v.as_object().is_some()) else {
                return IvyJson::Null;
            };
            let reset = v.get("resetsAt");
            let expired = reset.is_some_and(|r| !r.is_null() && value::number(r) <= now);
            let mut result = IvyJson::object([("windowDurationMins", IvyJson::Number(minutes))]);
            if expired {
                result.insert("usedPercent", IvyJson::Number(0.0));
                result.insert("resetsAt", IvyJson::Null);
            } else {
                if let Some(percent) = v.get("usedPercent") {
                    result.insert("usedPercent", percent.clone());
                }
                if let Some(reset) = reset {
                    result.insert("resetsAt", reset.clone());
                }
            }
            result
        };
        let reached = self.windows.values().any(|v| {
            field(v, "rejected").as_bool().unwrap_or(false)
                && (field(v, "resetsAt").is_null() || value::number(field(v, "resetsAt")) > now)
        });
        let bucket = |id: JsString, name: JsString, primary: IvyJson, secondary: IvyJson| {
            IvyJson::object([
                ("limitId", IvyJson::String(id.clone())),
                ("limitName", IvyJson::String(name)),
                ("primary", primary),
                ("secondary", secondary),
                ("credits", IvyJson::Null),
                ("planType", IvyJson::Null),
                (
                    "rateLimitReachedType",
                    if reached && id == JsString::from("claude-code") {
                        "rate_limit_reached".into()
                    } else {
                        IvyJson::Null
                    },
                ),
            ])
        };
        let current = bucket(
            "claude-code".into(),
            "Claude Code".into(),
            window(&"five_hour".into(), 300.0),
            window(&"seven_day".into(), 10080.0),
        );
        let mut by_id = BTreeMap::from([(JsString::from("claude-code"), current.clone())]);
        for key in self.windows.keys() {
            let label = if key == &JsString::from("seven_day_opus") {
                Some(JsString::from("Weekly · Opus"))
            } else if key == &JsString::from("seven_day_sonnet") {
                Some(JsString::from("Weekly · Sonnet"))
            } else if key == &JsString::from("seven_day_oauth_apps") {
                Some(JsString::from("Weekly · OAuth apps"))
            } else if key.0.starts_with(&JsString::from("model:").0) {
                let mut label = JsString::from("Weekly · ");
                label.0.extend_from_slice(&key.0[6..]);
                Some(label)
            } else {
                None
            };
            if let Some(label) = label {
                by_id.insert(
                    key.clone(),
                    bucket(key.clone(), label, window(key, 10080.0), IvyJson::Null),
                );
            }
        }
        Ok(IvyJson::object([
            ("rateLimits", current),
            ("rateLimitsByLimitId", IvyJson::object(by_id)),
        ]))
    }
}
