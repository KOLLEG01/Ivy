//! Retain and verify original turn pages before publishing a triage assessment.
use crate::{
    triage_native::{TriageCall, TriageNative},
    triage_results::{result_items, triage_turn},
    triage_store::{
        TriageDocument, TriageRecord as R, check, evidence, pin_value, same, text, triage_name,
    },
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts,
    json::{canonical, parse, try_js_string},
};
use ivy_sdk::{
    native_evidence::native_instant,
    native_turn_contents::{
        native_turn_snapshot, read_native_turn_contents, verify_native_turn_contents,
    },
};

pub(crate) fn read_params(thread: &JsString, full: bool) -> IvyJson {
    IvyJson::object([
        ("threadId", IvyJson::String(thread.clone())),
        ("cursor", IvyJson::Null),
        ("limit", IvyJson::Number(1.0)),
        ("sortDirection", "desc".into()),
        ("itemsView", if full { "full" } else { "notLoaded" }.into()),
    ])
}
fn read_name(
    plan: &TriageDocument,
    turn: &JsString,
    epoch: &JsString,
    full: bool,
) -> IvyResult<JsString> {
    triage_name(
        "read",
        &IvyJson::Array(vec![
            pin_value(&plan.pin),
            IvyJson::String(turn.clone()),
            IvyJson::String(epoch.clone()),
            full.into(),
        ]),
    )
}
fn decode(bytes: &[u8], maximum: usize) -> IvyResult<IvyJson> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    parse(
        std::str::from_utf8(bytes).map_err(|_| IvyError::internal())?,
        maximum,
    )
    .map_err(|_| IvyError::internal())
}
pub struct TriageCompletion<'a, 'b, 'engine, 'store, 'journal> {
    pub native: &'a TriageNative<'b, 'engine, 'store, 'journal>,
    pub plan: &'a TriageDocument,
    pub retained: &'a CheckedNativeContracts,
}
impl TriageCompletion<'_, '_, '_, '_, '_> {
    pub fn find(&self) -> IvyResult<Option<TriageDocument>> {
        self.native.store().find(
            R::Completion,
            &triage_name("completion", &pin_value(&self.plan.pin))?,
            true,
        )
    }
    fn maximum(&self) -> IvyResult<usize> {
        Ok(
            if self
                .plan
                .value
                .required("input")?
                .get("media")
                .is_some_and(crate::inbox_evidence::truthy)
            {
                8 * 1024 * 1024
            } else {
                2 * 1024 * 1024
            },
        )
    }
    fn turn_id(&self, turn: &TriageCall, thread: &JsString) -> IvyResult<JsString> {
        evidence(
            same(
                turn.stored.value.required("plan")?,
                &pin_value(&self.plan.pin),
            )? && turn.stored.value.get("method") == Some(&"turn/start".into())
                && turn.params.get("threadId").and_then(IvyJson::as_js_string) == Some(thread)
                && turn.observation.as_ref().and_then(|o| o.get("phase"))
                    == Some(&"succeeded".into()),
        )?;
        let observed = turn.observation.as_ref().unwrap();
        self.native
            .owner(self.plan)?
            .check_operation(observed, &turn.original()?)?;
        evidence(
            observed
                .get("reply")
                .is_some_and(|r| r.get("result").is_some()),
        )?;
        let id = observed
            .get("reply")
            .and_then(|r| r.get("result"))
            .and_then(|r| r.get("turn"))
            .and_then(|r| r.get("id"))
            .and_then(IvyJson::as_js_string);
        check(
            id.is_some_and(|id| !id.0.is_empty()),
            "secretary_triage_turn_changed",
        )?;
        Ok(id.unwrap().clone())
    }
    fn pair(
        &self,
        contents: &IvyJson,
        verification: &IvyJson,
        thread: &JsString,
        turn: &JsString,
    ) -> IvyResult<()> {
        let snapshot = native_turn_snapshot(contents);
        let last = if contents.get("format").is_some() {
            contents
                .required("pages")?
                .as_array()
                .and_then(|p| p.last())
                .ok_or_else(IvyError::internal)?
        } else {
            snapshot
        };
        self.native.owner(self.plan)?.check_read(
            verification,
            &"thread/turns/list".into(),
            &read_params(thread, false),
            &text(snapshot, "epoch")?,
            false,
        )?;
        let state = triage_turn(verification, turn, false)?;
        check(
            verification.get("observationId") != last.get("observationId")
                && native_instant(&text(verification, "observedAt")?, None)?
                    >= native_instant(&text(last, "observedAt")?, None)?
                && state.get("status") == Some(&"completed".into())
                && state.get("error").is_none_or(IvyJson::is_null),
            "secretary_triage_verification_changed",
        )
    }
    pub async fn collect(
        &self,
        turn: &TriageCall,
        thread: &JsString,
        epoch: &JsString,
    ) -> IvyResult<Option<TriageDocument>> {
        let owner = self.native.owner(self.plan)?;
        let turn_id = self.turn_id(turn, thread)?;
        let maximum = self.maximum()?;
        let name = read_name(self.plan, &turn_id, epoch, true)?;
        let (full_pin, full) = match self.native.store().binary(&name, None, maximum as f64)? {
            Some((pin, bytes)) => (pin, decode(&bytes, maximum)?),
            None => {
                let snapshot = owner
                    .read(
                        &"thread/turns/list".into(),
                        read_params(thread, false),
                        Some(epoch),
                        false,
                    )
                    .await?;
                let state = triage_turn(&snapshot, &turn_id, false)?
                    .get("status")
                    .cloned();
                if state == Some("inProgress".into()) {
                    return Ok(None);
                }
                let state = state
                    .as_ref()
                    .map(try_js_string)
                    .transpose()?
                    .unwrap_or_else(|| "undefined".into());
                check(
                    ["completed", "failed", "interrupted"]
                        .iter()
                        .any(|s| state == JsString::from(*s)),
                    "secretary_triage_turn_changed",
                )?;
                let full = read_native_turn_contents(
                    &owner,
                    self.retained,
                    &snapshot,
                    thread,
                    &turn_id,
                    Some(maximum),
                )
                .await?;
                self.native.guard(self.plan, false).await?;
                let bytes = canonical(&full, maximum)?.into_bytes();
                (
                    self.native
                        .store()
                        .save_binary(&name, &bytes, maximum as f64)?,
                    full,
                )
            }
        };
        owner.check_read(
            native_turn_snapshot(&full),
            &"thread/turns/list".into(),
            &read_params(thread, full.get("format").is_none()),
            epoch,
            false,
        )?;
        let verified =
            verify_native_turn_contents(&owner, self.retained, &full, thread, &turn_id).await?;
        let result = result_items(&verified.turn, &verified.items)?;
        let name = read_name(self.plan, &turn_id, epoch, false)?;
        let previous = self.native.store().binary(&name, None, maximum as f64)?;
        let verification = match &previous {
            Some((_, bytes)) => decode(bytes, maximum)?,
            None => {
                owner
                    .read(
                        &"thread/turns/list".into(),
                        read_params(thread, false),
                        Some(epoch),
                        false,
                    )
                    .await?
            }
        };
        self.pair(&full, &verification, thread, &turn_id)?;
        let verification_pin = match previous {
            Some((pin, _)) => pin,
            None => {
                self.native.guard(self.plan, false).await?;
                let bytes = canonical(&verification, maximum)?.into_bytes();
                self.native
                    .store()
                    .save_binary(&name, &bytes, maximum as f64)?
            }
        };
        self.native.guard(self.plan, false).await?;
        Ok(Some(self.native.store().create(
            R::Completion,
            &triage_name("completion", &pin_value(&self.plan.pin))?,
            &IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("plan", pin_value(&self.plan.pin)),
                ("turn", pin_value(&turn.stored.pin)),
                ("threadId", IvyJson::String(thread.clone())),
                ("turnId", IvyJson::String(turn_id)),
                ("full", pin_value(&full_pin)),
                ("verification", pin_value(&verification_pin)),
                ("result", result),
                ("completedAt", verification.required("observedAt")?.clone()),
            ]),
            true,
        )?))
    }
}
