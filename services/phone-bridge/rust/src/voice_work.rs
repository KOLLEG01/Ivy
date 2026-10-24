//! Durable backing turns are independent of the phone's media session.
use crate::journal::PhoneJournal;
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
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
fn increment(work: &mut Json) {
    work.insert(
        "revision",
        (work.get("revision").and_then(Json::as_f64).unwrap_or(0.0) + 1.0).into(),
    );
}
fn append_call(work: &mut Json, call: &Json) -> IvyResult<()> {
    let id = call.required("callId")?;
    let mut ids = work
        .required("callIds")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .to_vec();
    if !ids.contains(id) {
        ids.push(id.clone());
    }
    if ids.len() > 128 {
        ids.drain(..ids.len() - 128);
    }
    work.insert("callIds", Json::Array(ids));
    work.insert("attachedCallId", id.clone());
    Ok(())
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
#[derive(Clone, Default, PartialEq)]
struct Records {
    works: Vec<Json>,
    observations: Vec<Json>,
}
struct Inner {
    journal: PhoneJournal,
    records: Mutex<Records>,
    ready: tokio::sync::OnceCell<()>,
    serial: tokio::sync::Mutex<()>,
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
            records: Mutex::new(Records::default()),
            ready: tokio::sync::OnceCell::new(),
            serial: tokio::sync::Mutex::new(()),
            now,
        }))
    }
    pub fn now(&self) -> i64 {
        (self.0.now)()
    }
    pub async fn initialize(&self) -> IvyResult<()> {
        self.0
            .ready
            .get_or_try_init(|| async {
                let value = self.0.journal.voice_work_records().await?;
                let mut records = Records {
                    works: value
                        .required("works")?
                        .as_array()
                        .map(<[Json]>::to_vec)
                        .ok_or_else(IvyError::internal)?,
                    observations: value
                        .required("observations")?
                        .as_array()
                        .map(<[Json]>::to_vec)
                        .ok_or_else(IvyError::internal)?,
                };
                require(
                    records.works.len() <= 64 && records.observations.len() <= 64,
                    "phone_work_storage_invalid",
                )?;
                for work in &mut records.works {
                    for key in [
                        "threadId",
                        "turnId",
                        "fingerprint",
                        "principalId",
                        "partyKey",
                        "policyHash",
                    ] {
                        text(work, key).map_err(|_| {
                            IvyError::new(
                                "phone_work_storage_invalid",
                                "Saved Voice work identities are invalid.",
                            )
                        })?;
                    }
                    let ids = work
                        .required("callIds")?
                        .as_array()
                        .ok_or_else(IvyError::internal)?;
                    require(
                        ids.len() <= 128
                            && ids.iter().all(|id| id.as_js_string().is_some())
                            && state_is(
                                work,
                                &[
                                    "running",
                                    "waiting_input",
                                    "completed",
                                    "failed",
                                    "halting",
                                    "paused_deadline",
                                    "outcome_unknown",
                                ],
                            )
                            && work
                                .get("revision")
                                .and_then(Json::as_f64)
                                .is_some_and(|n| {
                                    n >= 0.0 && n.fract() == 0.0 && n <= 9_007_199_254_740_991.0
                                })
                            && work.get("deadline").is_some_and(|v| {
                                v.is_null() || v.as_f64().is_some_and(f64::is_finite)
                            }),
                        "phone_work_storage_invalid",
                    )?;
                    // A restart never grants another five minutes to a lost call.
                    if active(work) && work.get("attachedCallId").is_some_and(|v| !v.is_null()) {
                        work.insert("attachedCallId", Json::Null);
                        work.insert("endedAt", (self.now() as f64).into());
                        work.insert("deadline", (self.now() as f64).into());
                        increment(work);
                    }
                }
                for observation in &records.observations {
                    let id = text(observation, "callId")?;
                    let thread = text(observation, "threadId")?;
                    text(observation, "fingerprint")?;
                    let generation = observation
                        .required("generation")?
                        .as_f64()
                        .ok_or_else(IvyError::internal)?;
                    require(
                        generation >= 0.0
                            && generation.fract() == 0.0
                            && generation <= 128.0
                            && observation.get("endedAt").is_some_and(|v| {
                                v.is_null() || v.as_f64().is_some_and(f64::is_finite)
                            })
                            && self.0.journal.call(id.clone()).await?.is_some()
                            && self.0.journal.voice_task(id, generation as u32).await?
                                == Some(thread),
                        "phone_work_storage_invalid",
                    )?;
                }
                self.save(&records).await?;
                *self.0.records.lock().unwrap() = records;
                Ok::<_, IvyError>(())
            })
            .await?;
        Ok(())
    }
    async fn save(&self, records: &Records) -> IvyResult<()> {
        self.0
            .journal
            .save_voice_work_records(
                Json::Array(records.works.clone()),
                Json::Array(records.observations.clone()),
            )
            .await
    }
    async fn edit<T>(&self, change: impl FnOnce(&mut Records) -> IvyResult<T>) -> IvyResult<T> {
        self.initialize().await?;
        let _serial = self.0.serial.lock().await;
        let before = self.0.records.lock().unwrap().clone();
        let mut next = before.clone();
        let result = change(&mut next)?;
        while next.works.len() > 64 {
            let index = next
                .works
                .iter()
                .position(|w| state_is(w, &["completed", "failed"]));
            require(index.is_some(), "phone_work_capacity")?;
            next.works.remove(index.unwrap());
        }
        if next != before {
            self.save(&next).await?;
            *self.0.records.lock().unwrap() = next;
        }
        Ok(result)
    }
    pub fn all(&self) -> Vec<Json> {
        self.0.records.lock().unwrap().works.clone()
    }
    pub fn get(&self, id: &JsString) -> Option<Json> {
        self.all()
            .into_iter()
            .find(|w| w.get("threadId") == Some(&id.clone().into()))
    }
    pub fn for_call(&self, id: &JsString) -> Option<Json> {
        self.all().into_iter().rev().find(|w| {
            w.get("callIds")
                .and_then(Json::as_array)
                .is_some_and(|ids| ids.contains(&id.clone().into()))
        })
    }
    pub fn protected(&self, id: &JsString) -> bool {
        self.get(id).is_some_and(|w| {
            active(&w) || state_is(&w, &["halting", "paused_deadline", "outcome_unknown"])
        })
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
        self.0.records.lock().unwrap().observations.clone()
    }
    pub fn observation(&self, id: &JsString) -> Option<Json> {
        self.observations()
            .into_iter()
            .find(|o| o.get("threadId") == Some(&id.clone().into()))
    }
    pub fn has_work(&self) -> bool {
        !self.observations().is_empty()
            || self
                .all()
                .iter()
                .any(|w| active(w) || state_is(w, &["halting", "outcome_unknown"]))
    }
    pub async fn started(
        &self,
        call: Option<&Json>,
        thread: &JsString,
        turn: &JsString,
        fingerprint: &str,
        party: &str,
    ) -> IvyResult<()> {
        self.edit(|records| {
            if let Some(work) = records
                .works
                .iter_mut()
                .find(|w| w.get("threadId") == Some(&thread.clone().into()))
            {
                if work.get("turnId") == Some(&turn.clone().into()) {
                    return Ok(());
                }
                let forbidden = work.get("attachedCallId") == Some(&Json::Null) && call.is_none();
                work.insert("turnId", turn.clone().into());
                work.insert("state", "running".into());
                work.insert("error", Json::Null);
                work.insert("interruptId", Json::Null);
                if let Some(call) = call {
                    append_call(work, call)?;
                }
                if forbidden {
                    work.insert("deadline", (self.now() as f64).into());
                }
                increment(work);
            } else if let Some(call) = call {
                records.works.push(Json::object([
                    ("threadId", thread.clone().into()),
                    ("turnId", turn.clone().into()),
                    ("fingerprint", fingerprint.into()),
                    ("principalId", call.required("principalId")?.clone()),
                    ("partyKey", party.into()),
                    ("policyHash", call.required("policyHash")?.clone()),
                    (
                        "callIds",
                        Json::Array(vec![call.required("callId")?.clone()]),
                    ),
                    ("attachedCallId", call.required("callId")?.clone()),
                    ("state", "running".into()),
                    ("endedAt", Json::Null),
                    ("deadline", Json::Null),
                    ("revision", 0.0.into()),
                    ("interruptId", Json::Null),
                    ("error", Json::Null),
                ]));
            }
            Ok(())
        })
        .await
    }
    pub async fn completed(
        &self,
        thread: &JsString,
        turn: &JsString,
        status: &JsString,
    ) -> IvyResult<()> {
        self.edit(|records| {
            if let Some(work) = records.works.iter_mut().find(|w| {
                w.get("threadId") == Some(&thread.clone().into())
                    && w.get("turnId") == Some(&turn.clone().into())
            }) {
                let state = if status == &JsString::from("completed") {
                    "completed"
                } else if status == &JsString::from("interrupted")
                    && work.get("interruptId").is_some_and(|v| !v.is_null())
                {
                    "paused_deadline"
                } else {
                    "failed"
                };
                work.insert("state", state.into());
                increment(work);
            }
            Ok(())
        })
        .await
    }
    pub async fn waiting(&self, thread: &JsString) -> IvyResult<()> {
        self.edit(|records| {
            if let Some(work) = records.works.iter_mut().find(|w| {
                w.get("threadId") == Some(&thread.clone().into())
                    && w.get("state") == Some(&"running".into())
            }) {
                work.insert("state", "waiting_input".into());
                increment(work);
            }
            Ok(())
        })
        .await
    }
    pub async fn detach(&self, call: &JsString, ended: Option<i64>) -> IvyResult<()> {
        self.edit(|records| {
            if let Some(work) = records
                .works
                .iter_mut()
                .rev()
                .find(|w| w.get("attachedCallId") == Some(&call.clone().into()))
            {
                work.insert("attachedCallId", Json::Null);
                work.insert(
                    "endedAt",
                    ended.map(|n| Json::from(n as f64)).unwrap_or(Json::Null),
                );
                work.insert(
                    "deadline",
                    (ended
                        .map(|n| n.min(self.now()).saturating_add(WORK_WINDOW_MS))
                        .unwrap_or(self.now()) as f64)
                        .into(),
                );
                increment(work);
            }
            Ok(())
        })
        .await
    }
    pub fn rejoin(&self, call: &Json, fingerprint: &str, party: &str) -> IvyResult<Option<Json>> {
        let matches = self
            .all()
            .into_iter()
            .filter(|w| {
                active(w)
                    && w.get("attachedCallId") == Some(&Json::Null)
                    && w.get("principalId") == call.get("principalId")
                    && w.get("policyHash") == call.get("policyHash")
                    && w.get("fingerprint") == Some(&fingerprint.into())
                    && w.get("partyKey") == Some(&party.into())
                    && w.get("deadline")
                        .and_then(Json::as_f64)
                        .is_some_and(|n| (self.now() as f64) < n)
            })
            .collect::<Vec<_>>();
        require(matches.len() <= 1, "phone_work_ambiguous")?;
        Ok(matches.into_iter().next())
    }
    pub async fn attach(&self, thread: &JsString, call: &Json) -> IvyResult<()> {
        self.edit(|records| {
            if let Some(w) = records
                .works
                .iter_mut()
                .find(|w| w.get("threadId") == Some(&thread.clone().into()))
            {
                require(
                    w.get("principalId") == call.get("principalId")
                        && active(w)
                        && (w.get("attachedCallId") == call.get("callId")
                            || w.get("attachedCallId") == Some(&Json::Null)
                                && w.get("deadline")
                                    .and_then(Json::as_f64)
                                    .is_some_and(|n| (self.now() as f64) < n)),
                    "phone_work_unavailable",
                )?;
                append_call(w, call)?;
                w.insert("endedAt", Json::Null);
                w.insert("deadline", Json::Null);
                increment(w);
            }
            Ok(())
        })
        .await
    }
    pub async fn claim_expired(&self, force: bool) -> IvyResult<Vec<Json>> {
        self.edit(|records| {
            let mut claims = vec![];
            for w in &mut records.works {
                if state_is(w, &["halting"])
                    || active(w)
                        && (force
                            || w.get("attachedCallId") == Some(&Json::Null)
                                && w.get("deadline")
                                    .and_then(Json::as_f64)
                                    .is_some_and(|n| (self.now() as f64) >= n))
                {
                    if !state_is(w, &["halting"]) {
                        w.insert("state", "halting".into());
                        if w.get("interruptId").is_none_or(Json::is_null) {
                            w.insert("interruptId", uuid::Uuid::new_v4().to_string().into());
                        }
                        increment(w);
                    }
                    claims.push(w.clone());
                }
            }
            Ok(claims)
        })
        .await
    }
    pub async fn halt(&self, call: &JsString) -> IvyResult<Option<Json>> {
        self.edit(|records| {
            let Some(w) = records.works.iter_mut().rev().find(|w| {
                w.get("callIds")
                    .and_then(Json::as_array)
                    .is_some_and(|ids| ids.contains(&call.clone().into()))
            }) else {
                return Ok(None);
            };
            if !active(w)
                || w.get("attachedCallId")
                    .is_some_and(|id| !id.is_null() && Some(id) != Some(&call.clone().into()))
            {
                return Ok(None);
            }
            w.insert("state", "halting".into());
            if w.get("interruptId").is_none_or(Json::is_null) {
                w.insert("interruptId", uuid::Uuid::new_v4().to_string().into());
            }
            increment(w);
            Ok(Some(w.clone()))
        })
        .await
    }
    pub async fn interrupted(&self, claim: &Json, error: &str) -> IvyResult<()> {
        self.edit(|records| {
            if let Some(w) = records.works.iter_mut().find(|w| {
                w.get("threadId") == claim.get("threadId")
                    && w.get("interruptId") == claim.get("interruptId")
                    && state_is(w, &["halting"])
            }) {
                w.insert("state", "outcome_unknown".into());
                w.insert("error", error.into());
            }
            Ok(())
        })
        .await
    }
    pub async fn unknown(&self, thread: &JsString, error: &str) -> IvyResult<()> {
        self.edit(|records| {
            if let Some(w) = records
                .works
                .iter_mut()
                .find(|w| w.get("threadId") == Some(&thread.clone().into()))
            {
                w.insert("state", "outcome_unknown".into());
                w.insert("error", error.into());
                w.insert("attachedCallId", Json::Null);
                increment(w);
            }
            Ok(())
        })
        .await
    }
    pub async fn retain_observation(&self, observation: Json) -> IvyResult<()> {
        self.edit(|records| {
            if let Some(existing) = records
                .observations
                .iter_mut()
                .find(|o| o.get("threadId") == observation.get("threadId"))
            {
                *existing = observation;
            } else {
                require(records.observations.len() < 64, "phone_work_capacity")?;
                records.observations.push(observation);
            }
            Ok(())
        })
        .await
    }
    pub async fn observed(&self, thread: &JsString) -> IvyResult<()> {
        self.edit(|records| {
            records
                .observations
                .retain(|o| o.get("threadId") != Some(&thread.clone().into()));
            Ok(())
        })
        .await
    }
}
use ivy_sdk::clock::Clock;

#[cfg(test)]
#[path = "../../../../tests/rust/phone_voice_work.rs"]
mod tests;
