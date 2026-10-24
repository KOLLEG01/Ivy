//! Original journal argument and immutable request-hash guards.
use super::*;
pub(super) use crate::contracts::{validate, validate_admission, validate_native};
use ivy_contracts::{hash::digest, json::canonical, limits::MANAGEMENT_FRAME_BYTES};
pub(super) fn uuid(value: Option<&IvyJson>) -> IvyResult<()> {
    validate("Uuid", value.unwrap_or(&IvyJson::Null))
}
pub(super) fn field<'a>(value: &'a IvyJson, name: &str) -> IvyResult<&'a IvyJson> {
    value.get(name).ok_or_else(IvyError::internal)
}
pub(super) fn intent_shape(intent: &IvyJson) -> IvyResult<()> {
    validate("Intent", intent)?;
    let method = field(intent, "method")?.as_str()?;
    require(
        intent.get("voiceGeneration").is_none()
            || [
                "call.stopVoice",
                "call.realtime.prepare",
                "call.realtime.answer",
                "call.realtime.stop",
                "call.desktop.pauseVoice",
                "call.desktop.resumeVoice",
                "call.archiveVoice",
                "call.createVoice",
                "call.bindVoice",
                "call.promptVoice",
                "call.forwardVoice",
                "call.restartVoice",
            ]
            .contains(&method.as_str()),
        "invalid_arguments",
        "Only Voice generation operations may carry a generation.",
    )?;
    require(
        method.starts_with("call.") == (field(intent, "callId")? != &IvyJson::Null),
        "invalid_arguments",
        "Phone call commands require their original call identity.",
    )?;
    let hash_matches = |value: &IvyJson, maximum| -> IvyResult<bool> {
        Ok(intent.get("requestHash") == Some(&digest(canonical(value, maximum)?).into()))
    };
    match method.as_str() {
        "call.archiveVoice" => {
            let plan = field(intent, "archivePlan")?;
            require(
                hash_matches(plan, 32 * 1024)?,
                "phone_archive_plan_changed",
                "Archive intent must retain its exact original bounded plan.",
            )?;
            if let Some(thread_id) = plan.get("threadId") {
                let candidates = field(plan, "candidates")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?;
                require(
                    candidates.len() == 1 && candidates[0].get("threadId") == Some(thread_id),
                    "phone_archive_scope_conflict",
                    "A bound Voice archive plan must contain exactly its original task subtree.",
                )?;
            }
        }
        "call.selectVoice" => {
            let mut arguments = IvyJson::object(
                ["threadId", "prompt", "model", "reasoningEffort"]
                    .map(|name| Ok((name, field(intent, name)?.clone())))
                    .into_iter()
                    .collect::<IvyResult<Vec<_>>>()?,
            );
            if let Some(sequence) = intent.get("commandSequence") {
                arguments.insert("commandSequence", sequence.clone());
            }
            if let Some(voice) = intent.get("voiceId") {arguments.insert("voiceId",voice.clone());}
            require(
                hash_matches(&arguments, MANAGEMENT_FRAME_BYTES)?,
                "phone_voice_selection_changed",
                "Voice selection intent must retain its exact original arguments.",
            )?;
        }
        "call.restartVoice" => {
            let arguments = IvyJson::object(
                ["voiceGeneration", "model", "reasoningEffort"]
                    .map(|name| Ok((name, field(intent, name)?.clone())))
                    .into_iter()
                    .collect::<IvyResult<Vec<_>>>()?,
            );
            require(
                hash_matches(&arguments, MANAGEMENT_FRAME_BYTES)?,
                "phone_voice_restart_changed",
                "Voice restart must retain its original generation and selection.",
            )?;
        }
        "call.selectSpeakerVoice" => {
            let arguments = IvyJson::object([("voiceId", field(intent, "voiceId")?.clone())]);
            require(
                hash_matches(&arguments, MANAGEMENT_FRAME_BYTES)?,
                "phone_speaker_voice_changed",
                "Speaker selection must retain its original voice.",
            )?;
        }
        _ => (),
    }
    Ok(())
}
