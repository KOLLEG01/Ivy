//! Fresh capture inventory only. Session evidence never grants audio authority.
use super::{ActionResult, DesktopActionError};
use ivy_contracts::{IvyJson, json::JsString};
use std::{
    collections::HashSet,
    panic::{AssertUnwindSafe, catch_unwind},
};

pub fn validate_capture_text(value: &JsString) -> ActionResult<()> {
    if value.utf16_len() > 32767
        || value.0.contains(&0)
        || value
            .0
            .iter()
            .all(|c| char::from_u32(u32::from(*c)).is_some_and(char::is_whitespace))
    {
        return Err(DesktopActionError::Argument(
            "An exact native identifier is required.",
        ));
    }
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureSession {
    pub instance_id: JsString,
    pub state: &'static str,
    pub pid: u32,
    pub single_process: bool,
}
impl CaptureSession {
    pub fn new(
        instance_id: JsString,
        state: i32,
        pid: u32,
        process_result: i32,
    ) -> ActionResult<Self> {
        validate_capture_text(&instance_id)?;
        let state = match state {
            0 => "inactive",
            1 => "active",
            2 => "expired",
            _ => return Err(DesktopActionError::Argument("Invalid session state.")),
        };
        if process_result != 0 && process_result != 0x0889000d {
            return Err(DesktopActionError::Unavailable);
        }
        Ok(Self {
            instance_id,
            state,
            pid,
            single_process: process_result == 0 && pid != 0,
        })
    }
}
pub trait CaptureEndpoint {
    fn id(&mut self) -> ActionResult<JsString>;
    fn is_active_capture(&mut self) -> ActionResult<bool>;
    /// A provider must reject null inventories/elements rather than omit them.
    fn read_sessions(&mut self) -> ActionResult<Vec<CaptureSession>>;
    fn close(&mut self) -> ActionResult<()> {
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureObservation {
    pub endpoint_id: JsString,
    pub observed: bool,
    pub sessions: Vec<CaptureSession>,
}
fn guarded<T>(f: impl FnOnce() -> ActionResult<T>) -> ActionResult<T> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|_| DesktopActionError::Unavailable)?
}
impl CaptureObservation {
    pub fn read<E: CaptureEndpoint>(
        endpoint_id: &JsString,
        open: impl FnOnce(&JsString) -> ActionResult<Option<E>>,
    ) -> ActionResult<Self> {
        validate_capture_text(endpoint_id)?;
        let result = guarded(|| {
            let mut endpoint = open(endpoint_id)?.ok_or(DesktopActionError::Unavailable)?;
            let observation = guarded(|| {
                if endpoint.id()? != *endpoint_id || !endpoint.is_active_capture()? {
                    return Err(DesktopActionError::Unavailable);
                }
                let sessions = endpoint.read_sessions()?;
                if sessions.len() > 4096 {
                    return Err(DesktopActionError::Unavailable);
                }
                let mut ids = HashSet::new();
                for session in &sessions {
                    if !ids.insert(&session.instance_id) {
                        return Err(DesktopActionError::Unavailable);
                    }
                }
                if !endpoint.is_active_capture()? || endpoint.id()? != *endpoint_id {
                    return Err(DesktopActionError::Unavailable);
                }
                Ok(sessions)
            });
            // Dispose invalidates a positive observation too; a provider panic
            // must still retire the endpoint on its original worker.
            guarded(|| endpoint.close())?;
            observation
        });
        Ok(match result {
            Ok(sessions) => Self {
                endpoint_id: endpoint_id.clone(),
                observed: true,
                sessions,
            },
            Err(_) => Self {
                endpoint_id: endpoint_id.clone(),
                observed: false,
                sessions: vec![],
            },
        })
    }
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            ("endpointId", IvyJson::String(self.endpoint_id.clone())),
            (
                "state",
                (if self.observed {
                    "observed"
                } else {
                    "unavailable"
                })
                .into(),
            ),
            (
                "sessions",
                IvyJson::Array(
                    self.sessions
                        .iter()
                        .map(|s| {
                            IvyJson::object([
                                ("instanceId", IvyJson::String(s.instance_id.clone())),
                                ("state", s.state.into()),
                                ("pid", IvyJson::Number(f64::from(s.pid))),
                                ("singleProcess", s.single_process.into()),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
    }
}
