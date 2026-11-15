//! Durable backing turns are independent of the phone's media session.
use crate::journal::PhoneJournal;
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use ivy_sdk::clock::Clock;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
mod records;
pub(crate) use records::{
    Changes, OBSERVATION_BYTES, Observation, RECORD_LIMIT, Records, WORK_BYTES, WorkRecord,
    WorkState,
};

pub const WORK_WINDOW_MS: i64 = 300_000;
pub(crate) fn text(value: &Json, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn require(ok: bool, code: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(
            code,
            "Original Voice work is unavailable or requires reconciliation.",
        ))
    }
}
pub fn active(work: &Json) -> bool {
    state_is(work, &["running", "waiting_input"])
}
fn state_is(work: &Json, states: &[&str]) -> bool {
    states
        .iter()
        .any(|s| work.get("state") == Some(&(*s).into()))
}
pub fn view(work: &Json) -> IvyResult<Json> {
    let mut result = Json::object([] as [(&str, Json); 0]);
    for key in [
        "threadId",
        "turnId",
        "state",
        "attachedCallId",
        "endedAt",
        "deadline",
        "revision",
        "error",
    ] {
        result.insert(key, work.required(key)?.clone());
    }
    Ok(result)
}
struct Inner {
    journal: PhoneJournal,
    records: Arc<Mutex<Records>>,
    ready: tokio::sync::OnceCell<()>,
    changed: Arc<tokio::sync::Notify>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
}
#[derive(Clone)]
pub struct PhoneVoiceWorks(Arc<Inner>);
impl PhoneVoiceWorks {
    pub fn new(journal: PhoneJournal) -> Self {
        let epoch = ivy_sdk::clock::SystemClock.utc_millis();
        let started = Instant::now();
        Self::with_clock(
            journal,
            Arc::new(move || {
                epoch.saturating_add(started.elapsed().as_millis().min(i64::MAX as u128) as i64)
            }),
        )
    }
    pub fn with_clock(journal: PhoneJournal, now: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        Self(Arc::new(Inner {
            journal,
            records: Arc::new(Mutex::new(Records::default())),
            ready: tokio::sync::OnceCell::new(),
            changed: Arc::new(tokio::sync::Notify::new()),
            now,
        }))
    }
    pub fn now(&self) -> i64 {
        (self.0.now)()
    }
    pub async fn initialize(&self) -> IvyResult<()> {
        self.0
            .ready
            .get_or_try_init(|| {
                self.0
                    .journal
                    .initialize_voice_work_records(self.0.records.clone(), self.now())
            })
            .await?;
        Ok(())
    }
    async fn edit<T: Send + 'static>(
        &self,
        change: impl FnOnce(&Records) -> IvyResult<(Changes, T)> + Send + 'static,
    ) -> IvyResult<T> {
        self.initialize().await?;
        // Commit and cache publication belong to the existing journal worker.
        // Dropping an admitted caller cannot leave a stale in-memory row.
        self.0
            .journal
            .edit_voice_work_records(self.0.records.clone(), self.0.changed.clone(), change)
            .await
    }
    async fn edit_work(
        &self,
        thread: JsString,
        change: impl FnOnce(&mut WorkRecord) -> IvyResult<()> + Send + 'static,
    ) -> IvyResult<()> {
        self.edit(move |records| {
            let Some(prior) = records.get(&thread) else {
                return Ok((Changes::default(), ()));
            };
            let mut next = prior.clone();
            change(&mut next)?;
            if next == *prior {
                return Ok((Changes::default(), ()));
            }
            next.bump()?;
            Ok((Changes::work(next), ()))
        })
        .await
    }
    pub fn all(&self) -> Vec<Json> {
        self.0
            .records
            .lock()
            .unwrap()
            .works
            .iter()
            .map(WorkRecord::to_wire)
            .collect()
    }
    pub fn get(&self, id: &JsString) -> Option<Json> {
        self.0
            .records
            .lock()
            .unwrap()
            .get(id)
            .map(WorkRecord::to_wire)
    }
    pub fn for_call(&self, id: &JsString) -> Option<Json> {
        self.0
            .records
            .lock()
            .unwrap()
            .for_call(id)
            .map(WorkRecord::to_wire)
    }
    pub fn protected(&self, id: &JsString) -> bool {
        self.0
            .records
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|w| !w.state.terminal())
    }
    pub fn can_answer(&self, work: &Json) -> bool {
        active(work)
            && (work.get("attachedCallId").is_some_and(|v| !v.is_null())
                || work
                    .get("deadline")
                    .and_then(Json::as_f64)
                    .is_some_and(|n| (self.now() as f64) < n))
    }
    pub fn observations(&self) -> Vec<Json> {
        self.0
            .records
            .lock()
            .unwrap()
            .observations
            .iter()
            .map(Observation::to_wire)
            .collect()
    }
    pub fn observation(&self, id: &JsString) -> Option<Json> {
        self.0
            .records
            .lock()
            .unwrap()
            .observation(id)
            .map(Observation::to_wire)
    }
    pub(crate) fn due_observation(
        &self,
        retry_at: &std::collections::BTreeMap<JsString, i64>,
    ) -> Option<Json> {
        let now = self.now();
        self.0
            .records
            .lock()
            .unwrap()
            .observations
            .iter()
            .find(|o| retry_at.get(&o.thread).copied().unwrap_or(0) <= now)
            .map(Observation::to_wire)
    }
    pub fn has_work(&self) -> bool {
        let records = self.0.records.lock().unwrap();
        !records.observations.is_empty() || records.works.iter().any(|w| w.state.needs_owner())
    }
    pub(crate) async fn changed(&self) {
        self.0.changed.notified().await;
    }
    pub(crate) fn wake(&self) {
        self.0.changed.notify_one();
    }
    pub(crate) fn next_deadline(&self) -> Option<Duration> {
        let now = self.now();
        self.0
            .records
            .lock()
            .unwrap()
            .works
            .iter()
            .filter(|w| w.state.active() && w.attached.is_none())
            .filter_map(|w| w.deadline)
            .min()
            .map(|at| Duration::from_millis(at.saturating_sub(now).max(0) as u64))
    }
    pub async fn started(
        &self,
        call: Option<&Json>,
        thread: &JsString,
        turn: &JsString,
        fingerprint: &str,
        party: &str,
    ) -> IvyResult<()> {
        let (thread, turn, fingerprint, party) = (
            thread.clone(),
            turn.clone(),
            JsString::from(fingerprint),
            JsString::from(party),
        );
        let call = call
            .map(|c| {
                Ok::<_, IvyError>((
                    text(c, "callId")?,
                    text(c, "principalId")?,
                    text(c, "policyHash")?,
                ))
            })
            .transpose()?;
        let now = self.now();
        self.edit(move |records| {
            if let Some(prior) = records.get(&thread) {
                if prior.turn == turn {
                    return Ok((Changes::default(), ()));
                }
                let mut work = prior.clone();
                let forbidden = work.attached.is_none() && call.is_none();
                work.turn = turn;
                work.state = WorkState::Running;
                work.error = None;
                work.interrupt = None;
                if let Some((call, _, _)) = call {
                    work.attach_call(call);
                }
                if forbidden {
                    work.deadline = Some(now);
                }
                work.bump()?;
                Ok((Changes::work(work), ()))
            } else if let Some((call, principal, policy)) = call {
                let remove_work = if records.works.len() == RECORD_LIMIT {
                    Some(
                        records
                            .works
                            .iter()
                            .find(|w| w.state.terminal())
                            .ok_or_else(|| {
                                IvyError::new(
                                    "phone_work_capacity",
                                    "Voice work capacity is occupied.",
                                )
                            })?
                            .thread
                            .clone(),
                    )
                } else {
                    None
                };
                let work = WorkRecord {
                    stored_bytes: 0,
                    thread,
                    turn,
                    fingerprint,
                    principal,
                    party,
                    policy,
                    calls: vec![call.clone()],
                    attached: Some(call),
                    state: WorkState::Running,
                    ended: None,
                    deadline: None,
                    revision: 0,
                    interrupt: None,
                    error: None,
                };
                Ok((
                    Changes {
                        remove_work,
                        ..Changes::work(work)
                    },
                    (),
                ))
            } else {
                Ok((Changes::default(), ()))
            }
        })
        .await
    }
    pub async fn completed(
        &self,
        thread: &JsString,
        turn: &JsString,
        status: &JsString,
    ) -> IvyResult<()> {
        let (turn, status) = (turn.clone(), status.clone());
        self.edit_work(thread.clone(), move |w| {
            if w.turn == turn {
                w.state = if status == JsString::from("completed") {
                    WorkState::Completed
                } else if status == JsString::from("interrupted") && w.interrupt.is_some() {
                    WorkState::PausedDeadline
                } else {
                    WorkState::Failed
                };
                w.error = None;
            }
            Ok(())
        })
        .await
    }
    pub async fn waiting(&self, thread: &JsString) -> IvyResult<()> {
        self.edit_work(thread.clone(), |w| {
            if w.state == WorkState::Running {
                w.state = WorkState::WaitingInput;
            }
            Ok(())
        })
        .await
    }
    pub async fn detach(&self, call: &JsString, ended: Option<i64>) -> IvyResult<()> {
        let (call, now) = (call.clone(), self.now());
        self.edit(move |records| {
            let Some(prior) = records
                .works
                .iter()
                .rev()
                .find(|w| w.attached.as_ref() == Some(&call))
            else {
                return Ok((Changes::default(), ()));
            };
            let mut work = prior.clone();
            work.attached = None;
            work.ended = ended;
            work.deadline = Some(
                ended
                    .map(|n| n.min(now).saturating_add(WORK_WINDOW_MS))
                    .unwrap_or(now),
            );
            work.bump()?;
            Ok((Changes::work(work), ()))
        })
        .await
    }
    pub fn rejoin(&self, call: &Json, fingerprint: &str, party: &str) -> IvyResult<Option<Json>> {
        let (principal, policy) = (text(call, "principalId")?, text(call, "policyHash")?);
        let (fingerprint, party, now) = (
            JsString::from(fingerprint),
            JsString::from(party),
            self.now(),
        );
        let records = self.0.records.lock().unwrap();
        let mut found = None;
        for work in &records.works {
            if work.state.active()
                && work.attached.is_none()
                && work.principal == principal
                && work.policy == policy
                && work.fingerprint == fingerprint
                && work.party == party
                && work.deadline.is_some_and(|n| now < n)
            {
                require(found.is_none(), "phone_work_ambiguous")?;
                found = Some(work);
            }
        }
        Ok(found.map(WorkRecord::to_wire))
    }
    pub async fn attach(&self, thread: &JsString, call: &Json) -> IvyResult<()> {
        let (call, principal, now) = (
            text(call, "callId")?,
            text(call, "principalId")?,
            self.now(),
        );
        self.edit_work(thread.clone(), move |w| {
            require(
                w.principal == principal
                    && w.state.active()
                    && (w.attached.as_ref() == Some(&call)
                        || w.attached.is_none() && w.deadline.is_some_and(|n| now < n)),
                "phone_work_unavailable",
            )?;
            w.attach_call(call);
            w.ended = None;
            w.deadline = None;
            Ok(())
        })
        .await
    }
    pub async fn claim_expired(&self, force: bool) -> IvyResult<Vec<Json>> {
        self.initialize().await?;
        let now = self.now();
        let due = move |w: &WorkRecord| {
            w.state == WorkState::Halting
                || w.state.active()
                    && (force || w.attached.is_none() && w.deadline.is_some_and(|n| now >= n))
        };
        // No worker message, allocation or transaction for an idle deadline check.
        if !self.0.records.lock().unwrap().works.iter().any(due) {
            return Ok(Vec::new());
        }
        self.edit(move |records| {
            let mut changes = Changes::default();
            let mut claims = Vec::new();
            for prior in records.works.iter().filter(|w| due(w)) {
                if prior.state == WorkState::Halting {
                    claims.push(prior.to_wire());
                } else {
                    let mut work = prior.clone();
                    work.claim()?;
                    claims.push(work.to_wire());
                    changes.works.push(work);
                }
            }
            Ok((changes, claims))
        })
        .await
    }
    pub async fn halt(&self, call: &JsString) -> IvyResult<Option<Json>> {
        let call = call.clone();
        self.edit(move |records| {
            let Some(prior) = records
                .for_call(&call)
                .filter(|w| w.state.active() && w.attached.as_ref().is_none_or(|id| id == &call))
            else {
                return Ok((Changes::default(), None));
            };
            let mut work = prior.clone();
            work.claim()?;
            let result = work.to_wire();
            Ok((Changes::work(work), Some(result)))
        })
        .await
    }
    /// Admit one dispatch durably before touching the native transport. An
    /// uncertain dispatch is observed after restart, never issued a second time.
    pub async fn begin_interrupt(&self, claim: &Json) -> IvyResult<bool> {
        let (thread, turn, interrupt) = (
            text(claim, "threadId")?,
            text(claim, "turnId")?,
            text(claim, "interruptId")?,
        );
        self.edit(move |records| {
            let Some(prior) = records.get(&thread).filter(|w| {
                w.state == WorkState::Halting
                    && w.turn == turn
                    && w.interrupt.as_ref() == Some(&interrupt)
            }) else {
                return Ok((Changes::default(), false));
            };
            let mut work = prior.clone();
            work.state = WorkState::OutcomeUnknown;
            work.error = Some("phone_work_interrupt_pending".into());
            work.bump()?;
            Ok((Changes::work(work), true))
        })
        .await
    }
    pub async fn interrupted(&self, claim: &Json, error: &str) -> IvyResult<()> {
        let (interrupt, error) = (text(claim, "interruptId")?, JsString::from(error));
        self.edit_work(text(claim, "threadId")?, move |w| {
            if w.interrupt.as_ref() == Some(&interrupt)
                && (w.state == WorkState::Halting
                    || w.state == WorkState::OutcomeUnknown
                        && w.error.as_ref() == Some(&"phone_work_interrupt_pending".into()))
            {
                w.state = WorkState::OutcomeUnknown;
                w.error = Some(error);
            }
            Ok(())
        })
        .await
    }
    pub async fn unknown(&self, thread: &JsString, error: &str) -> IvyResult<()> {
        let error = JsString::from(error);
        self.edit_work(thread.clone(), move |w| {
            w.state = WorkState::OutcomeUnknown;
            w.error = Some(error);
            w.attached = None;
            Ok(())
        })
        .await
    }
    pub async fn retain_observation(&self, observation: Json) -> IvyResult<()> {
        let mut observation = Observation::from_wire(&observation)?;
        self.edit(move |records| {
            let prior = records.observation(&observation.thread);
            observation.stored_bytes = prior.map_or(0, |o| o.stored_bytes);
            require(
                prior.is_some() || records.observations.len() < RECORD_LIMIT,
                "phone_work_capacity",
            )?;
            let changes = if prior == Some(&observation) {
                Changes::default()
            } else {
                Changes {
                    observation: Some(observation),
                    ..Changes::default()
                }
            };
            Ok((changes, ()))
        })
        .await
    }
    pub async fn observed(&self, thread: &JsString) -> IvyResult<()> {
        let thread = thread.clone();
        self.edit(move |records| {
            Ok((
                Changes {
                    remove_observation: records.observation(&thread).map(|_| thread),
                    ..Changes::default()
                },
                (),
            ))
        })
        .await
    }
}
#[cfg(test)]
#[path = "../../../../tests/rust/phone_voice_work.rs"]
mod tests;
