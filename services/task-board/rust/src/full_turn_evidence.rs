//! A full native turn is a distinct proof, admitted only by the original
//! unsupported first item request and an independent terminal verification.
use crate::{
    evidence_bytes::MAXIMUM_BYTES,
    native_turn_evidence::{
        Completion, Transcript, TurnEvidence, TurnEvidenceStore, transcript_summary,
    },
    operations::Store,
    read_evidence::ReadIdentity,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, json::canonical, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::clock::parse_date;
use std::collections::HashSet;

pub fn full_turn_params(thread: &JsString, cursor: Option<&JsString>, full: bool) -> Json {
    Json::object([
        ("threadId", Json::String(thread.clone())),
        (
            "cursor",
            cursor.cloned().map(Json::String).unwrap_or(Json::Null),
        ),
        ("limit", Json::Number(1.0)),
        ("itemsView", if full { "full" } else { "notLoaded" }.into()),
        ("sortDirection", "desc".into()),
    ])
}
pub fn first_items_params(thread: &JsString, turn: &JsString) -> Json {
    Json::object([
        ("threadId", Json::String(thread.clone())),
        ("turnId", Json::String(turn.clone())),
        ("cursor", Json::Null),
        ("limit", Json::Number(100.0)),
        ("sortDirection", "asc".into()),
    ])
}
pub struct FullTurnEvidence<'a, S> {
    pub turns: TurnEvidence<'a, S>,
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn string(v: &Json) -> JsString {
    v.as_js_string().cloned().unwrap_or_default()
}
fn array(v: &Json) -> &[Json] {
    v.as_array().unwrap_or(&[])
}

impl<S: TurnEvidenceStore> FullTurnEvidence<'_, S> {
    async fn read(
        &self,
        value: &Json,
        initial: &Json,
        reference: &Json,
        method: &str,
        params: Json,
    ) -> IvyResult<Json> {
        require(
            reference.get("epoch") == initial.get("epoch"),
            "task_board_native_epoch_changed",
            "Full evidence must retain one native epoch.",
        )?;
        self.turns
            .reads()
            .read(
                value.required("run")?,
                reference.required("artifact")?,
                &ReadIdentity {
                    observation_id: s(reference, "observationId")?.clone(),
                    caller_principal_id: self.turns.principal_id.clone(),
                    service_node_id: s(initial, "serviceNodeId")?.clone(),
                    native_version: s(initial, "nativeVersion")?.clone(),
                    epoch: s(initial, "epoch")?.clone(),
                    method: method.into(),
                    params,
                },
            )
            .await
    }
    fn select<'a>(&self, observation: &'a Json, initial: &Json) -> IvyResult<&'a Json> {
        let reply = observation.required("reply")?;
        require(
            reply.get("result").is_some(),
            "task_board_native_evidence_required",
            "A full transcript requires successful original native turn reads.",
        )?;
        let data = array(field(reply.required("result")?, "data"));
        require(
            data.len() == 1,
            "task_board_turn_mismatch",
            "The exact full-turn page must contain one turn.",
        )?;
        let turn = &data[0];
        require(
            turn.get("id") == initial.get("turnId") && turn.get("status") == initial.get("status"),
            "task_board_native_state_changed",
            "The original terminal turn must remain unchanged.",
        )?;
        Ok(turn)
    }
    pub async fn verify(&self, run: &Json, value: &Json) -> IvyResult<Completion> {
        self.turns
            .store
            .validate("NativeFullTurnTranscript", value)?;
        let snapshot = self
            .turns
            .snapshot(run, value.required("snapshot")?)
            .await?;
        let initial = &snapshot.document.value;
        require(
            canonical(value.required("run")?, MANAGEMENT_FRAME_BYTES)?
                == canonical(initial.required("run")?, MANAGEMENT_FRAME_BYTES)?
                && ["completed", "failed", "interrupted"]
                    .iter()
                    .any(|v| initial.get("status") == Some(&Json::from(*v))),
            "task_board_evidence_mismatch",
            "Full evidence requires the original terminal snapshot and Run scope.",
        )?;
        let unsupported = self
            .read(
                value,
                initial,
                value.required("unsupportedItems")?,
                "thread/items/list",
                first_items_params(s(initial, "threadId")?, s(initial, "turnId")?),
            )
            .await?;
        require(
            field(unsupported.required("reply")?, "error")
                .get("code")
                .and_then(Json::as_f64)
                == Some(-32601.0),
            "task_board_native_evidence_required",
            "Fallback requires the exact native unsupported-method error, not an arbitrary failed read.",
        )?;
        let full = self
            .read(
                value,
                initial,
                value.required("fullTurn")?,
                "thread/turns/list",
                full_turn_params(
                    s(initial, "threadId")?,
                    value.get("fullTurnCursor").and_then(Json::as_js_string),
                    true,
                ),
            )
            .await?;
        let verification = self
            .read(
                value,
                initial,
                value.required("verification")?,
                "thread/turns/list",
                full_turn_params(
                    s(initial, "threadId")?,
                    value.get("verificationCursor").and_then(Json::as_js_string),
                    false,
                ),
            )
            .await?;
        let turn = self.select(&full, initial)?;
        self.select(&verification, initial)?;
        require(
            turn.get("itemsView") == Some(&Json::from("full")),
            "task_board_native_evidence_required",
            "Only an explicitly complete native item view can publish a full transcript.",
        )?;
        require(
            Some(canonical(&full, MAXIMUM_BYTES)?.len() as f64)
                == value.get("fullTurnBytes").and_then(Json::as_f64),
            "task_board_evidence_mismatch",
            "Full native observation bytes must match their retained length.",
        )?;
        let mut previous = parse_date(s(initial, "createdAt")?);
        let mut identities = HashSet::from([
            s(initial.required("metadata")?, "observationId")?.clone(),
            s(initial.required("turn")?, "observationId")?.clone(),
        ]);
        for observed in [&unsupported, &full, &verification] {
            let at = parse_date(s(observed, "observedAt")?);
            require(
                at.is_finite()
                    && at >= previous
                    && identities.insert(s(observed, "observationId")?.clone()),
                "task_board_evidence_mismatch",
                "Full transcript observations must be successive, distinct and follow the terminal snapshot.",
            )?;
            previous = at;
        }
        require(
            value.get("createdAt") == verification.get("observedAt"),
            "task_board_evidence_mismatch",
            "Completion time belongs to the final native verification.",
        )?;
        let mut ids = HashSet::new();
        let mut text = JsString::default();
        let mut last = JsString::default();
        let mut answers = 0;
        for item in array(field(turn, "items")) {
            let id = string(field(item, "id"));
            require(
                !id.0.is_empty() && ids.insert(id),
                "task_board_evidence_mismatch",
                "Complete native items must retain unique nonempty identities.",
            )?;
            if item.get("type") == Some(&Json::from("agentMessage")) {
                last = string(field(item, "text"));
                if item.get("phase") == Some(&Json::from("final_answer")) {
                    if answers > 0 {
                        text.0.extend([10, 10]);
                    }
                    text.0.extend_from_slice(&last.0);
                    answers += 1;
                }
            }
        }
        if text.0.is_empty() {
            text = last;
        }
        if text.0.is_empty() {
            text = format!("Native turn {}.", s(initial, "status")?.to_utf8()?).into();
        }
        Ok(Completion {
            status: s(initial, "status")?.clone(),
            snapshot,
            summary: transcript_summary(text),
        })
    }
    pub async fn transcript(&self, run: &Json, artifact: &Json) -> IvyResult<Transcript> {
        self.turns.store.validate("Artifact", artifact)?;
        let document = self
            .turns
            .store
            .read_document(
                "task-board/native-full-turn-transcript",
                artifact.required("object")?,
                false,
            )
            .await?;
        require(
            document.parent_id.as_ref() == Some(s(run, "objectId")?)
                && artifact.get("contentHash") == Some(&Json::from(document.content_hash.clone()))
                && artifact.get("mediaType") == Some(&Json::from("application/json")),
            "task_board_evidence_mismatch",
            "The full transcript must retain its exact immutable artifact and parent.",
        )?;
        let completion = self.verify(run, &document.value).await?;
        Ok(Transcript {
            document,
            completion,
        })
    }
}
pub async fn verify_native_transcript<S: TurnEvidenceStore + Store>(
    turns: TurnEvidence<'_, S>,
    run: &Json,
    artifact: &Json,
) -> IvyResult<Transcript> {
    let key = turns
        .store
        .local_key(s(artifact.required("object")?, "objectId")?)?;
    if key.as_deref() == Some("task-board/native-full-turn-transcript") {
        FullTurnEvidence { turns }.transcript(run, artifact).await
    } else {
        turns.transcript(run, artifact).await
    }
}
