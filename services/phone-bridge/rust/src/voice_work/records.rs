use super::*;

pub(crate) const RECORD_LIMIT: usize = 64;
pub(crate) const WORK_BYTES: usize = 256 * 1024;
pub(crate) const OBSERVATION_BYTES: usize = 64 * 1024;
const CALL_LIMIT: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkState {
    Running,
    WaitingInput,
    Completed,
    Failed,
    Halting,
    PausedDeadline,
    OutcomeUnknown,
}
impl WorkState {
    fn name(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::WaitingInput => "waiting_input",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Halting => "halting",
            Self::PausedDeadline => "paused_deadline",
            Self::OutcomeUnknown => "outcome_unknown",
        }
    }
    fn parse(value: &Json) -> IvyResult<Self> {
        match value.as_str()?.as_str() {
            "running" => Ok(Self::Running),
            "waiting_input" => Ok(Self::WaitingInput),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "halting" => Ok(Self::Halting),
            "paused_deadline" => Ok(Self::PausedDeadline),
            "outcome_unknown" => Ok(Self::OutcomeUnknown),
            _ => Err(invalid()),
        }
    }
    pub(crate) fn active(self) -> bool {
        matches!(self, Self::Running | Self::WaitingInput)
    }
    pub(crate) fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
    pub(crate) fn needs_owner(self) -> bool {
        self.active() || matches!(self, Self::Halting | Self::OutcomeUnknown)
    }
}
fn invalid() -> IvyError {
    IvyError::new(
        "phone_work_storage_invalid",
        "Saved Voice work identities are invalid.",
    )
}
fn optional_text(value: &Json, key: &str) -> IvyResult<Option<JsString>> {
    match value.required(key)? {
        Json::Null => Ok(None),
        Json::String(s) => Ok(Some(s.clone())),
        _ => Err(invalid()),
    }
}
fn time(value: &Json, key: &str) -> IvyResult<Option<i64>> {
    let value = value.required(key)?;
    if value.is_null() {
        return Ok(None);
    }
    let number = value.as_f64().ok_or_else(invalid)?;
    require(
        number.is_finite() && number.fract() == 0.0 && number.abs() <= 9_007_199_254_740_991.0,
        "phone_work_storage_invalid",
    )?;
    Ok(Some(number as i64))
}
fn wire_text(value: &Option<JsString>) -> Json {
    value.clone().map(Json::String).unwrap_or(Json::Null)
}
fn wire_time(value: Option<i64>) -> Json {
    value.map(|n| Json::from(n as f64)).unwrap_or(Json::Null)
}

#[derive(Clone, PartialEq)]
pub(crate) struct WorkRecord {
    pub stored_bytes: usize,
    pub thread: JsString,
    pub turn: JsString,
    pub fingerprint: JsString,
    pub principal: JsString,
    pub party: JsString,
    pub policy: JsString,
    pub calls: Vec<JsString>,
    pub attached: Option<JsString>,
    pub state: WorkState,
    pub ended: Option<i64>,
    pub deadline: Option<i64>,
    pub revision: u64,
    pub interrupt: Option<JsString>,
    pub error: Option<JsString>,
}
impl WorkRecord {
    pub(crate) fn from_wire(value: &Json) -> IvyResult<Self> {
        let revision = value.required("revision")?.as_f64().ok_or_else(invalid)?;
        require(
            revision >= 0.0 && revision.fract() == 0.0 && revision <= 9_007_199_254_740_991.0,
            "phone_work_storage_invalid",
        )?;
        let calls = value.required("callIds")?.as_array().ok_or_else(invalid)?;
        require(calls.len() <= CALL_LIMIT, "phone_work_storage_invalid")?;
        Ok(Self {
            stored_bytes: 0,
            thread: text(value, "threadId")?,
            turn: text(value, "turnId")?,
            fingerprint: text(value, "fingerprint")?,
            principal: text(value, "principalId")?,
            party: text(value, "partyKey")?,
            policy: text(value, "policyHash")?,
            calls: calls
                .iter()
                .map(|id| id.as_js_string().cloned().ok_or_else(invalid))
                .collect::<IvyResult<_>>()?,
            attached: optional_text(value, "attachedCallId")?,
            state: WorkState::parse(value.required("state")?)?,
            ended: time(value, "endedAt")?,
            deadline: time(value, "deadline")?,
            revision: revision as u64,
            interrupt: optional_text(value, "interruptId")?,
            error: optional_text(value, "error")?,
        })
    }
    pub(crate) fn to_wire(&self) -> Json {
        Json::object([
            ("threadId", self.thread.clone().into()),
            ("turnId", self.turn.clone().into()),
            ("fingerprint", self.fingerprint.clone().into()),
            ("principalId", self.principal.clone().into()),
            ("partyKey", self.party.clone().into()),
            ("policyHash", self.policy.clone().into()),
            (
                "callIds",
                Json::Array(self.calls.iter().cloned().map(Json::String).collect()),
            ),
            ("attachedCallId", wire_text(&self.attached)),
            ("state", self.state.name().into()),
            ("endedAt", wire_time(self.ended)),
            ("deadline", wire_time(self.deadline)),
            ("revision", (self.revision as f64).into()),
            ("interruptId", wire_text(&self.interrupt)),
            ("error", wire_text(&self.error)),
        ])
    }
    pub(crate) fn attach_call(&mut self, call: JsString) {
        if !self.calls.contains(&call) {
            if self.calls.len() == CALL_LIMIT {
                self.calls.remove(0);
            }
            self.calls.push(call.clone());
        }
        self.attached = Some(call);
    }
    pub(crate) fn bump(&mut self) -> IvyResult<()> {
        require(self.revision < 9_007_199_254_740_991, "phone_work_capacity")?;
        self.revision += 1;
        Ok(())
    }
    pub(crate) fn claim(&mut self) -> IvyResult<()> {
        self.state = WorkState::Halting;
        if self.interrupt.is_none() {
            self.interrupt = Some(uuid::Uuid::new_v4().to_string().into());
        }
        self.bump()
    }
}

#[derive(Clone, PartialEq)]
pub(crate) struct Observation {
    pub stored_bytes: usize,
    pub thread: JsString,
    pub call: JsString,
    pub fingerprint: JsString,
    pub generation: u32,
    pub ended: Option<i64>,
}
impl Observation {
    pub(crate) fn from_wire(value: &Json) -> IvyResult<Self> {
        let generation = value.required("generation")?.as_f64().ok_or_else(invalid)?;
        require(
            generation.fract() == 0.0 && (0.0..=128.0).contains(&generation),
            "phone_work_storage_invalid",
        )?;
        Ok(Self {
            stored_bytes: 0,
            thread: text(value, "threadId")?,
            call: text(value, "callId")?,
            fingerprint: text(value, "fingerprint")?,
            generation: generation as u32,
            ended: time(value, "endedAt")?,
        })
    }
    pub(crate) fn to_wire(&self) -> Json {
        Json::object([
            ("threadId", self.thread.clone().into()),
            ("callId", self.call.clone().into()),
            ("fingerprint", self.fingerprint.clone().into()),
            ("generation", (self.generation as f64).into()),
            ("endedAt", wire_time(self.ended)),
        ])
    }
}

#[derive(Default)]
pub(crate) struct Records {
    pub initialized: bool,
    pub work_bytes: usize,
    pub observation_bytes: usize,
    pub works: Vec<WorkRecord>,
    pub observations: Vec<Observation>,
}
impl Records {
    pub(crate) fn get(&self, thread: &JsString) -> Option<&WorkRecord> {
        self.works.iter().find(|w| &w.thread == thread)
    }
    pub(crate) fn for_call(&self, call: &JsString) -> Option<&WorkRecord> {
        self.works.iter().rev().find(|w| w.calls.contains(call))
    }
    pub(crate) fn observation(&self, thread: &JsString) -> Option<&Observation> {
        self.observations.iter().find(|o| &o.thread == thread)
    }
}
#[derive(Default)]
pub(crate) struct Changes {
    pub works: Vec<WorkRecord>,
    pub remove_work: Option<JsString>,
    pub observation: Option<Observation>,
    pub remove_observation: Option<JsString>,
}
impl Changes {
    pub(crate) fn work(work: WorkRecord) -> Self {
        Self {
            works: vec![work],
            ..Self::default()
        }
    }
    pub(crate) fn apply(self, records: &mut Records) {
        if let Some(id) = self.remove_work {
            records.works.retain(|w| w.thread != id);
        }
        for work in self.works {
            if let Some(prior) = records.works.iter_mut().find(|w| w.thread == work.thread) {
                *prior = work;
            } else {
                records.works.push(work);
            }
        }
        if let Some(id) = self.remove_observation {
            records.observations.retain(|o| o.thread != id);
        }
        if let Some(observation) = self.observation {
            if let Some(prior) = records
                .observations
                .iter_mut()
                .find(|o| o.thread == observation.thread)
            {
                *prior = observation;
            } else {
                records.observations.push(observation);
            }
        }
    }
    pub(crate) fn empty(&self) -> bool {
        self.works.is_empty()
            && self.remove_work.is_none()
            && self.observation.is_none()
            && self.remove_observation.is_none()
    }
}
