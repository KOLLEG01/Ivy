//! Original uninterrupted read/check/admit boundaries run on the journal worker.
//! Native observation and dispatch still belong to the Phone call-command owner.
use super::*;
use crate::admission::PhoneAdmission;
use ivy_contracts::{hash::digest, json::canonical};

pub(crate) enum NativeAdmission {
    Outgoing(IvyJson),
    Incoming(IvyJson),
}
pub(crate) struct NativeCommandSnapshot {
    pub call: IvyJson,
    pub prior: Option<IvyJson>,
    pub request_hash: String,
    pub key: u32,
    pub current: bool,
}
fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .get(key)
        .and_then(IvyJson::as_js_string)
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn require_index(value: Option<&IvyJson>, feedback: bool) -> IvyResult<u32> {
    let number = value.and_then(IvyJson::as_f64).unwrap_or(f64::NAN);
    require(
        number.is_finite()
            && number.fract() == 0.0
            && if feedback {
                number >= 1.0 && number <= 2_147_483_647.0
            } else {
                number >= 0.0 && number <= 128.0
            },
        "invalid_arguments",
        if feedback {
            "Positive bounded command feedback sequence required."
        } else {
            "Bounded Voice generation required."
        },
    )?;
    Ok(number as u32)
}
impl PhoneJournal {
    /// Preserve the flow's original uninterrupted current-call/command/policy
    /// check. The asynchronous service must not split these reads into jobs.
    pub(crate) async fn flow_continuing(
        &self,
        policy: PhoneAdmission,
        call: IvyJson,
        native_epoch: JsString,
    ) -> IvyResult<()> {
        self.request(move |db| {
            let id = text(&call, "callId")?;
            require(
                call.get("epoch").and_then(IvyJson::as_js_string) == Some(&native_epoch)
                    && db
                        .current_call(Some(&id))?
                        .as_ref()
                        .and_then(|v| v.get("callId"))
                        == Some(&id.clone().into())
                    && db.call_command(&id, &"call.hangup".into(), 0)?.is_none()
                    && db.call_command(&id, &"call.release".into(), 0)?.is_none(),
                "phone_call_cancelled",
                "The original call is no longer admitted for positive work.",
            )?;
            policy.authorize(&text(&call, "principalId")?, &call, "connect")
        })
        .await
    }
    pub(crate) async fn authorized_call(
        &self,
        policy: PhoneAdmission,
        principal: JsString,
        call_id: JsString,
        action: &'static str,
    ) -> IvyResult<IvyJson> {
        self.request(move |db| {
            let call = db.call(&call_id)?.ok_or_else(|| {
                IvyError::new(
                    "phone_call_missing",
                    "Original call admission is unavailable.",
                )
            })?;
            policy.authorize(&principal, &call, action)?;
            Ok(call)
        })
        .await
    }
    pub(crate) async fn admit_native_observation(
        &self,
        policy: PhoneAdmission,
        request: NativeAdmission,
        native_epoch: JsString,
        observed: IvyJson,
    ) -> IvyResult<Option<IvyJson>> {
        self.request(move |db| {
            let args = match &request {
                NativeAdmission::Outgoing(v) | NativeAdmission::Incoming(v) => v,
            };
            let operation_id = text(args, "operationId")?;
            if db.call_for_operation(&operation_id)?.is_some() {
                return Ok(None);
            }
            require(
                db.epoch()?.as_deref() == Some(native_epoch.to_utf8()?.as_str()),
                "phone_epoch_mismatch",
                "The observed native Phone epoch has retired.",
            )?;
            let admission = match request {
                NativeAdmission::Outgoing(admission) => {
                    let count = observed
                        .get("callIds")
                        .and_then(IvyJson::as_array)
                        .map_or_else(
                            || usize::from(observed.get("call").is_some_and(|v| !v.is_null())),
                            |ids| ids.len(),
                        );
                    require(
                        observed.get("configured") == Some(&true.into())
                            && observed.get("endpoint").is_some_and(|v| !v.is_null())
                            && count < db.max_concurrent_calls,
                        "phone_call_busy",
                        "Native Phone must have a configured free call slot.",
                    )?;
                    admission
                }
                NativeAdmission::Incoming(args) => {
                    let call_id = text(&args, "callId")?;
                    let no_call = IvyJson::Null;
                    let call = observed.get("call").unwrap_or(&no_call);
                    require(
                        observed.get("configured") == Some(&true.into())
                            && call.get("id") == Some(&call_id.into()),
                        "phone_call_conflict",
                        "The requested incoming call is no longer the original native call.",
                    )?;
                    let mut admission = policy.incoming(
                        native_epoch.into(),
                        operation_id.into(),
                        args.get("principalId").cloned().unwrap_or(IvyJson::Null),
                        call,
                    )?;
                    if let Some(route) = args
                        .get("route")
                        .and_then(IvyJson::as_js_string)
                        .filter(|s| !s.0.is_empty())
                    {
                        admission.insert("route", route.clone().into());
                    }
                    admission
                }
            };
            db.admit_call(admission).map(Some)
        })
        .await
    }
    pub(crate) async fn native_command_snapshot(
        &self,
        policy: PhoneAdmission,
        principal: JsString,
        call_id: JsString,
        method: String,
        params: IvyJson,
        native_epoch: JsString,
    ) -> IvyResult<NativeCommandSnapshot> {
        self.request(move |db| {
            let call = db.call(&call_id)?.ok_or_else(|| {
                IvyError::new(
                    "phone_call_missing",
                    "Original call admission is unavailable.",
                )
            })?;
            let cleanup = [
                "call.hangup",
                "call.release",
                "call.desktop.stopVoice",
                "call.realtime.stop",
            ]
            .contains(&method.as_str());
            policy.authorize(
                &principal,
                &call,
                if cleanup { "cleanup" } else { "connect" },
            )?;
            let request_hash = digest(&canonical(
                &IvyJson::object([
                    ("method", method.clone().into()),
                    ("params", params.clone()),
                ]),
                65536,
            )?);
            let feedback = method == "call.command.feedback";
            let generation = method.starts_with("call.realtime.")
                || ["call.desktop.pauseVoice", "call.desktop.resumeVoice"]
                    .contains(&method.as_str());
            let key = if feedback {
                require_index(params.get("commandSequence"), true)?
            } else if generation {
                require_index(params.get("generation"), false)?
            } else {
                0
            };
            let prior = if feedback {
                db.sequence_command(&call_id, key, false)?
            } else {
                db.call_command(&call_id, &method.into(), key)?
            };
            let current = prior.is_none()
                && call.get("epoch") == Some(&native_epoch.into())
                && call.get("epoch") == Some(&db.epoch_json()?)
                && db.current_call(Some(&call_id))?.is_some();
            Ok(NativeCommandSnapshot {
                call,
                prior,
                request_hash,
                key,
                current,
            })
        })
        .await
    }
}
