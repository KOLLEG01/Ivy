use ivy_contracts::IvyJson;

/// Bounded telemetry only. Retained effect outcomes live in the command journal.
/// The serialized service owner supplies a single ISO timestamp for each observation.
pub struct PhoneDiagnostics {
    service_node_id: String,
    calls: Vec<(String, IvyJson)>,
    cleanups: Vec<(String, IvyJson)>,
    observations: Vec<(String, IvyJson)>,
    poll: Option<IvyJson>,
    voice: Option<IvyJson>,
    registration: Option<IvyJson>,
}

impl PhoneDiagnostics {
    pub fn new(service_node_id: String) -> Self {
        Self {
            service_node_id,
            calls: Vec::with_capacity(32),
            cleanups: Vec::with_capacity(32),
            observations: Vec::with_capacity(64),
            poll: None,
            voice: None,
            registration: None,
        }
    }

    fn observation(&self, code: &str, prior: Option<&IvyJson>, scope: &str, at: &str) -> IvyJson {
        let first = prior
            .filter(|value| value.get("code") == Some(&code.into()))
            .and_then(|value| value.get("firstObservedAt"))
            .cloned()
            .unwrap_or_else(|| at.into());
        let message = match scope {
            "call_cleanup" => "Phone media cleanup is unconfirmed. Inspect the original stop and release outcomes.",
            "work_observation" => "The original Voice work status is awaiting reconciliation; call media cleanup is independent.",
            "call" => {
                "The original Phone call needs attention. Inspect its retained command outcomes; do not repeat the call."
            }
            "registration" => {
                "Phone SIP registration or its keepalive needs attention. Inspect phone.status; existing call outcomes remain available."
            }
            "codex_voice" => {
                "Codex Voice is unavailable. SIP remains available; inspect the configured CLI runtime and its original task outcome."
            }
            _ => "Phone observation is unavailable. Original command outcomes remain available.",
        };
        IvyJson::object([
            ("code", code.into()),
            (
                "resource",
                IvyJson::object([
                    ("serviceNodeId", self.service_node_id.clone().into()),
                    ("scope", scope.into()),
                ]),
            ),
            ("severity", "warning".into()),
            ("source", "phone-bridge".into()),
            ("firstObservedAt", first),
            ("lastObservedAt", at.into()),
            ("status", "current".into()),
            ("message", message.into()),
        ])
    }

    pub fn call_failed(&mut self, call_id: &str, code: &str, at: &str) {
        let index = self.calls.iter().position(|(id, _)| id == call_id);
        let next = self.observation(code, index.map(|index| &self.calls[index].1), "call", at);
        if let Some(index) = index {
            self.calls[index].1 = next;
        } else {
            // Updating an existing call preserves Map insertion order, not LRU order.
            if self.calls.len() == 32 {
                self.calls.remove(0);
            }
            self.calls.push((call_id.to_owned(), next));
        }
    }

    /// Cleanup or a successful poll is not recovery proof. A connected call is.
    pub fn call_succeeded(&mut self) {
        self.calls.clear();
    }
    pub fn cleanup_observed(&mut self, call_id: &str, code: Option<&str>, at: &str) {
        let prior = self.cleanups.iter().find(|(id, _)| id == call_id).map(|(_, value)| value);
        let next = code.map(|code| self.observation(code, prior, "call_cleanup", at));
        update(&mut self.cleanups, call_id, next, 32);
    }
    pub fn work_observed(&mut self, call_id: &str, code: Option<&str>, at: &str) {
        let prior = self.observations.iter().find(|(id, _)| id == call_id).map(|(_, value)| value);
        let next = code.map(|code| self.observation(code, prior, "work_observation", at));
        update(&mut self.observations, call_id, next, 64);
    }
    pub fn poll_failed(&mut self, code: &str, at: &str) {
        self.poll = Some(self.observation(code, self.poll.as_ref(), "poll", at));
    }
    pub fn poll_succeeded(&mut self) {
        self.poll = None;
    }
    pub fn voice_failed(&mut self, code: &str, at: &str) -> bool {
        let changed = self.voice.as_ref().and_then(|value| value.get("code")) != Some(&code.into());
        self.voice = Some(self.observation(code, self.voice.as_ref(), "codex_voice", at));
        changed
    }
    pub fn voice_succeeded(&mut self) -> bool {
        self.voice.take().is_some()
    }

    pub fn registration_observed(&mut self, enabled: bool, value: Option<&IvyJson>, at: &str) {
        let code = if !enabled {
            None
        } else if value.and_then(|value| value.get("state")) != Some(&"registered".into()) {
            Some("phone_registration_unavailable")
        } else if value
            .and_then(|value| value.get("lastKeepAliveError"))
            .and_then(IvyJson::as_js_string)
            .is_some_and(|error| !error.0.is_empty())
        {
            Some("phone_registration_keepalive_failed")
        } else {
            None
        };
        self.registration =
            code.map(|code| self.observation(code, self.registration.as_ref(), "registration", at));
    }

    /// Snapshot is detached from mutable state; current/released call IDs do not filter warnings.
    pub fn snapshot(&self) -> Vec<IvyJson> {
        self.calls
            .iter()
            .map(|(_, value)| value)
            .chain(self.cleanups.iter().map(|(_, value)| value))
            .chain(self.observations.iter().map(|(_, value)| value))
            .chain(self.poll.iter())
            .chain(self.voice.iter())
            .chain(self.registration.iter())
            .cloned()
            .collect()
    }
}
fn update(rows: &mut Vec<(String, IvyJson)>, id: &str, next: Option<IvyJson>, maximum: usize) {
    match (rows.iter().position(|(key, _)| key == id), next) {
        (Some(index), Some(value)) => rows[index].1 = value,
        (Some(index), None) => { rows.remove(index); },
        (None, Some(value)) => { if rows.len() == maximum { rows.remove(0); } rows.push((id.to_owned(), value)); },
        (None, None) => (),
    }
}
