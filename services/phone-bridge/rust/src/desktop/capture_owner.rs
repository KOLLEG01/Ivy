//! Correlate two fresh inventories against one retained process snapshot.
use super::audio_process::guarded;
use super::{
    ActionResult, AudioProcessOwnership, AudioProcessRecord, AudioProcessSnapshot,
    AudioProcessState, CaptureObservation, CaptureSession, DesktopActionError, ProcessImageEqual,
    validate_capture_text,
};
use crate::media::AudioDesktopIdentity;
use ivy_contracts::{IvyJson, json::JsString};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopCaptureIdentity {
    pub instance_id: JsString,
    pub pid: i32,
    pub process_start_ticks: i64,
}
#[derive(Clone, Debug)]
pub struct DesktopCaptureCorrelation {
    pub endpoint_id: JsString,
    pub state: &'static str,
    pub identity: Option<DesktopCaptureIdentity>,
}
struct Processes<'a, 'e, S> {
    source: Option<&'a mut S>,
    equal: &'e ProcessImageEqual<'e>,
    records: HashMap<i32, AudioProcessRecord>,
    order: Vec<i32>,
}
impl<S: AudioProcessSnapshot> AudioProcessSnapshot for Processes<'_, '_, S> {
    fn read(&mut self, pid: i32) -> ActionResult<Option<AudioProcessRecord>> {
        let Some(record) = self
            .source
            .as_mut()
            .ok_or(DesktopActionError::Unavailable)?
            .read(pid)?
        else {
            return Ok(None);
        };
        if let Some(old) = self.records.get(&pid) {
            if !old.same(Some(&record), self.equal) {
                return Err(DesktopActionError::Unavailable);
            }
        } else {
            self.order.push(pid);
        }
        self.records.insert(pid, record.clone());
        Ok(Some(record))
    }
    // Borrowed. The enclosing observation alone closes the actual source.
}
fn active(
    endpoint: &JsString,
    observation: Option<CaptureObservation>,
) -> ActionResult<Vec<CaptureSession>> {
    let observed = observation.ok_or(DesktopActionError::Unavailable)?;
    if !observed.observed || observed.endpoint_id != *endpoint || observed.sessions.len() > 4096 {
        return Err(DesktopActionError::Unavailable);
    }
    let mut ids = HashSet::new();
    if observed
        .sessions
        .iter()
        .any(|s| !ids.insert(&s.instance_id))
    {
        return Err(DesktopActionError::Unavailable);
    }
    let mut active: Vec<_> = observed
        .sessions
        .into_iter()
        .filter(|s| s.state == "active")
        .collect();
    active.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
    if active.len() > 32 {
        return Err(DesktopActionError::Unavailable);
    }
    Ok(active)
}
fn same(left: &[CaptureSession], right: &[CaptureSession]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(a, b)| {
            a.instance_id == b.instance_id && a.pid == b.pid && a.single_process == b.single_process
        })
}
impl DesktopCaptureCorrelation {
    pub fn read<S: AudioProcessSnapshot>(
        root: &AudioDesktopIdentity,
        endpoint: &JsString,
        current: &dyn Fn() -> ActionResult<bool>,
        mut capture: impl FnMut(&JsString) -> ActionResult<Option<CaptureObservation>>,
        open: impl FnOnce() -> ActionResult<Option<S>>,
        equal: &ProcessImageEqual<'_>,
    ) -> ActionResult<Self> {
        validate_capture_text(endpoint)?;
        let unavailable = || Self {
            endpoint_id: endpoint.clone(),
            state: "unavailable",
            identity: None,
        };
        Ok(guarded(|| {
            if !current()? {
                return Ok(unavailable());
            }
            let initial = active(endpoint, capture(endpoint)?)?;
            if initial
                .iter()
                .any(|s| !s.single_process || s.pid == 0 || s.pid > i32::MAX as u32)
            {
                return Ok(Self {
                    endpoint_id: endpoint.clone(),
                    state: "ambiguous",
                    identity: None,
                });
            }
            let mut source = open()?;
            let result = {
                let mut processes = Processes {
                    source: source.as_mut(),
                    equal,
                    records: HashMap::new(),
                    order: vec![],
                };
                guarded(|| {
                    let mut owned = HashMap::new();
                    let mut visited = HashSet::new();
                    for session in &initial {
                        let pid = session.pid as i32;
                        if !visited.insert(pid) {
                            continue;
                        }
                        let ownership = AudioProcessOwnership::read(
                            root,
                            pid,
                            current,
                            || Ok(Some(&mut processes)),
                            equal,
                        )?;
                        match ownership.state {
                            AudioProcessState::Unavailable => {
                                return Err(DesktopActionError::Unavailable);
                            }
                            AudioProcessState::Owned => {
                                owned.insert(
                                    pid,
                                    processes
                                        .read(pid)?
                                        .ok_or(DesktopActionError::Unavailable)?,
                                );
                            }
                            AudioProcessState::Foreign => {}
                        }
                    }
                    let final_capture = active(endpoint, capture(endpoint)?)?;
                    if !same(&initial, &final_capture) {
                        return Err(DesktopActionError::Unavailable);
                    }
                    for key in &processes.order {
                        let record = &processes.records[key];
                        if !record.same(
                            processes
                                .source
                                .as_mut()
                                .ok_or(DesktopActionError::Unavailable)?
                                .read(record.pid)?
                                .as_ref(),
                            equal,
                        ) {
                            return Err(DesktopActionError::Unavailable);
                        }
                    }
                    if !current()? {
                        return Err(DesktopActionError::Unavailable);
                    }
                    let matches: Vec<_> = final_capture
                        .iter()
                        .filter(|s| owned.contains_key(&(s.pid as i32)))
                        .collect();
                    Ok(match matches.as_slice() {
                        [] => Self {
                            endpoint_id: endpoint.clone(),
                            state: "none",
                            identity: None,
                        },
                        [session] => Self {
                            endpoint_id: endpoint.clone(),
                            state: "matched",
                            identity: Some(DesktopCaptureIdentity {
                                instance_id: session.instance_id.clone(),
                                pid: session.pid as i32,
                                process_start_ticks: owned[&(session.pid as i32)].start_ticks,
                            }),
                        },
                        _ => Self {
                            endpoint_id: endpoint.clone(),
                            state: "ambiguous",
                            identity: None,
                        },
                    })
                })
            };
            if let Some(source) = source.as_mut() {
                guarded(|| source.close())?;
            }
            let result = result?;
            Ok(if current()? { result } else { unavailable() })
        })
        .unwrap_or_else(|_| unavailable()))
    }
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            ("endpointId", IvyJson::String(self.endpoint_id.clone())),
            ("state", self.state.into()),
            (
                "identity",
                self.identity.as_ref().map_or(IvyJson::Null, |i| {
                    IvyJson::object([
                        ("instanceId", IvyJson::String(i.instance_id.clone())),
                        ("pid", IvyJson::Number(i.pid.into())),
                        (
                            "processStartTimeUtcTicks",
                            i.process_start_ticks.to_string().into(),
                        ),
                    ])
                }),
            ),
        ])
    }
}
