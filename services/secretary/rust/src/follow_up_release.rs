//! Completed answers survive caller-scoped release and uncertain original receipts.
use super::*;
use crate::journal::{Journal, Pin};
use ivy_contracts::{Outcome, agent_schema::validate_agent};

#[cfg(test)]
#[path = "../../../../tests/secretary-rust/follow-up-release.rs"]
mod tests;

fn pending(value: &IvyJson) -> bool {
    ["pending", "observing", "outcome_unknown"]
        .iter()
        .any(|s| field(value, "state").is_str(s))
}
pub(super) fn pending_for_item(journal: &Journal, item: &IvyJson, except: &str) -> IvyResult<bool> {
    let item = canonical(item, 1_048_576)?;
    journal.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM technical_documents WHERE key='secretary/follow-up' AND name<>? AND json_extract(value_json,'$.request.itemId')=json_extract(?,'$') AND json_extract(value_json,'$.contextRelease.state') IN ('pending','observing','outcome_unknown'))",
        rusqlite::params![except,item], |r| r.get(0),
    ).map_err(sqlite::error)
}
pub(super) fn plan(
    pin: &Pin,
    context: &IvyJson,
    question: &IvyJson,
    status: &IvyJson,
) -> IvyResult<Option<IvyJson>> {
    let start = field(context, "start");
    if !field(start, "method").is_str("thread/start")
        || !field(field(start, "params"), "serviceName").is_str("secretary")
        || !field(field(start, "params"), "threadSource").is_str("ivy-secretary-follow-up")
        || !field(context, "createdFor")
            .as_js_string()
            .is_some_and(|v| !v.0.is_empty())
        || field(field(context, "target"), "backend").is_null()
        || !field(question, "phase").is_str("succeeded")
        || field(question, "answer").is_null()
        || field(context, "threadId") != field(question, "threadId")
    {
        return Ok(None);
    }
    let original = text(question.required("request")?, "operationId")?;
    Ok(Some(IvyJson::object([
        ("state", "pending".into()),
        (
            "context",
            IvyJson::object([
                ("objectId", pin.object_id.clone().into()),
                ("revision", IvyJson::Number(pin.revision as f64)),
            ]),
        ),
        ("target", context.required("target")?.clone()),
        ("threadId", context.required("threadId")?.clone()),
        (
            "operationId",
            IvyJson::String(derive_operation_id_js(
                &original,
                &"release-secretary-context".into(),
            )?),
        ),
        ("expectedEpoch", status.required("epoch")?.clone()),
        (
            "expectedOperationId",
            IvyJson::String(derive_operation_id_js(
                &original,
                &"secretary-follow-up-turn".into(),
            )?),
        ),
        ("call", IvyJson::Null),
        ("errorCode", IvyJson::Null),
    ])))
}

impl SecretaryFollowUp<'_, '_, '_> {
    fn release_eligible(&self, saved: &Document, context: Option<&Document>) -> IvyResult<bool> {
        let Some(context) = context else {
            return Ok(false);
        };
        let journal = self.engine.evidence.store.journal;
        let latest = saved;
        let item = canonical(
            saved.value.required("request")?.required("itemId")?,
            1_048_576,
        )?;
        let (question_revision, context_revision, busy): (Option<i64>, Option<i64>, bool) = journal.db.query_row(
            "SELECT (SELECT current_revision FROM technical_documents WHERE id=?1 AND key='secretary/follow-up'),(SELECT current_revision FROM technical_documents WHERE id=?2 AND key='secretary/follow-up-context'),EXISTS(SELECT 1 FROM technical_documents WHERE key='secretary/follow-up' AND name<>?3 AND json_extract(value_json,'$.request.itemId')=json_extract(?4,'$') AND COALESCE(json_extract(value_json,'$.phase'),'') NOT IN ('succeeded','failed'))",
            rusqlite::params![saved.pin.object_id,context.pin.object_id,saved.name,item], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).map_err(sqlite::error)?;
        Ok(question_revision == Some(saved.pin.revision)
            && context_revision == Some(context.pin.revision)
            && field(field(&latest.value, "contextRelease"), "context").get("objectId")
                == Some(&context.pin.object_id.clone().into())
            && field(
                field(field(&latest.value, "contextRelease"), "context"),
                "revision",
            )
            .as_f64()
                == Some(context.pin.revision as f64)
            && field(field(&latest.value, "contextRelease"), "target")
                == field(&context.value, "target")
            && field(&latest.value, "phase").is_str("succeeded")
            && latest.value.get("threadId") == context.value.get("threadId")
            && !busy
            && !journal.follow_up_busy(&text(&context.value, "threadId")?, &saved.name)?)
    }
    fn save_release(&self, saved: &mut Document, release: IvyJson) -> IvyResult<()> {
        if saved.value.get("contextRelease") == Some(&release) {
            return Ok(());
        }
        let mut next = saved.value.clone();
        next.insert("contextRelease", release);
        *saved =
            self.engine
                .evidence
                .store
                .journal
                .amend_technical(FOLLOW_UP_KEY, &saved.pin, &next)?;
        Ok(())
    }
    pub(super) async fn release_context(&self, saved: &mut Document) -> IvyResult<()> {
        let mut release = field(&saved.value, "contextRelease").clone();
        if !pending(&release) {
            return Ok(());
        }
        let journal = self.engine.evidence.store.journal;
        let context_pin = release.required("context")?;
        let context = match journal.read(
            CONTEXT_KEY,
            &context_pin.required("objectId")?.as_str()?,
            None,
            true,
        ) {
            Ok(value) => Some(value),
            Err(error) if error.code == "not_found" => None,
            Err(error) => return Err(error),
        };
        let owner = self.owner(release.required("target")?)?;
        if field(&release, "call").is_null() {
            if !self.release_eligible(saved, context.as_ref())? {
                release.insert("state", "skipped".into());
                return self.save_release(saved, release);
            }
            let binding = owner.binding(&"thread/unsubscribe".into(), None).await?;
            self.engine.evidence.verify_owner().await?;
            if !self.release_eligible(saved, context.as_ref())? {
                release.insert("state", "skipped".into());
                return self.save_release(saved, release);
            }
            let call = NativeOperationCall {
                operation_id: text(&release, "operationId")?,
                method: "agent.release".into(),
                params: IvyJson::object([
                    ("threadId", release.required("threadId")?.clone()),
                    ("expectedEpoch", release.required("expectedEpoch")?.clone()),
                    (
                        "expectedOperationId",
                        release.required("expectedOperationId")?.clone(),
                    ),
                ]),
                definition_hash: text(binding.value(), "definitionHash")?,
            };
            let mut input = call.params.clone();
            input.insert("operationId", IvyJson::String(call.operation_id.clone()));
            input.insert(
                "nativeVersion",
                release
                    .required("target")?
                    .required("nativeVersion")?
                    .clone(),
            );
            input.insert(
                "expectedDefinitionHash",
                IvyJson::String(call.definition_hash.clone()),
            );
            input.insert(
                "backend",
                release.required("target")?.required("backend")?.clone(),
            );
            validate_agent("ThreadReleaseInput", &input)?;
            release.insert("call", call_json(&call));
            self.save_release(saved, release.clone())?;
        }
        let call = retained_call(release.required("call")?)?;
        let observed = owner.operation(&call).await?;
        let result =
            if observed.get("phase").is_none() && field(&release, "state").is_str("pending") {
                if !self.release_eligible(saved, context.as_ref())? {
                    release.insert("state", "skipped".into());
                    return self.save_release(saved, release);
                }
                // An interrupted send is observed by its original identity on restart;
                // an absent receipt after that point never authorizes a replacement.
                release.insert("state", "observing".into());
                self.save_release(saved, release.clone())?;
                owner
                    .dispatch_local(&call, || async {
                        self.engine.evidence.verify_owner().await?;
                        need(
                            self.release_eligible(saved, context.as_ref())?,
                            "secretary_follow_up_release_changed",
                            "New work or a changed context retains the native subscription.",
                        )
                    })
                    .await
            } else {
                Ok(Some(observed))
            };
        let observed = match result {
            Ok(Some(value)) => value,
            Ok(None) => return Ok(()),
            Err(error) => {
                release.insert(
                    "state",
                    if error.outcome == Outcome::NotExecuted {
                        "blocked"
                    } else {
                        "outcome_unknown"
                    }
                    .into(),
                );
                release.insert("errorCode", error.code.clone().into());
                self.save_release(saved, release)?;
                return if error.outcome == Outcome::NotExecuted {
                    Ok(())
                } else {
                    Err(error)
                };
            }
        };
        if observed.get("phase").is_some() {
            owner.check_operation(&observed, &call)?;
        }
        let state = if field(&observed, "phase").is_str("succeeded") {
            "released"
        } else if field(&observed, "phase").is_str("failed") {
            "blocked"
        } else if field(&observed, "phase").is_str("outcome_unknown") {
            "outcome_unknown"
        } else if observed.get("phase").is_none() {
            "outcome_unknown"
        } else {
            "observing"
        };
        release.insert("state", state.into());
        release.insert(
            "errorCode",
            if observed.get("phase").is_none() {
                "secretary_follow_up_release_absent".into()
            } else {
                field(&observed, "code").clone()
            },
        );
        self.save_release(saved, release.clone())?;
        if state == "outcome_unknown" {
            return Err(IvyError::new(
                field(&release, "errorCode")
                    .as_str()
                    .unwrap_or_else(|_| "secretary_follow_up_release_unknown".into()),
                "The original context release remains unresolved.",
            )
            .with_outcome(Outcome::Unknown));
        }
        Ok(())
    }
}
