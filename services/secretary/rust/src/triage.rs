//! Dormant bounded triage admission, assessment recovery and follow-up context.
use crate::{
    inbox_recovery::RecoveryIssues,
    triage_completion::TriageCompletion,
    triage_native::TriageNative,
    triage_store::{TriageDocument, TriageRecord as R, check, evidence, pin_value, same, text},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, checked_native_contract::CheckedNativeContracts,
};
use ivy_sdk::{clock::date_to_iso, rpc::RequestOptions};
use std::{cell::RefCell, sync::Arc};

pub struct SecretaryTriage<'a, 'engine, 'store, 'journal> {
    pub native: TriageNative<'a, 'engine, 'store, 'journal>,
    pub retained: &'a CheckedNativeContracts,
    item_cursor: RefCell<Option<IvyJson>>,
    active: RefCell<Option<Arc<tokio::sync::OnceCell<IvyResult<()>>>>>,
}
impl<'a, 'engine, 'store, 'journal> SecretaryTriage<'a, 'engine, 'store, 'journal> {
    pub fn new(
        native: TriageNative<'a, 'engine, 'store, 'journal>,
        retained: &'a CheckedNativeContracts,
    ) -> Self {
        Self {
            native,
            retained,
            item_cursor: RefCell::new(None),
            active: RefCell::new(None),
        }
    }
    fn finish(&self, work: &TriageDocument, value: &IvyJson) -> IvyResult<()> {
        if let Err(error) = self.native.store().amend(R::Work, work, value) {
            if !self
                .native
                .work(work.value.required("item")?)?
                .is_some_and(|saved| same(&saved.value, value).unwrap_or(false))
            {
                return Err(error);
            }
        }
        Ok(())
    }
    fn retain_context(
        &self,
        work: &TriageDocument,
        plan: &TriageDocument,
        completion: &TriageDocument,
    ) -> IvyResult<()> {
        let store = self.native.store();
        let saved = store.journal.read(
            R::Completion.key(),
            &completion.pin.object_id,
            Some(completion.pin.revision),
            true,
        )?;
        let now = self.native.engine.clock.utc_millis();
        let settings = plan.value.required("settings")?;
        store.journal.create_technical(
            "secretary/follow-up-context",
            &text(work.value.required("item")?, "objectId")?.to_utf8()?,
            &IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                (
                    "itemId",
                    work.value.required("item")?.required("objectId")?.clone(),
                ),
                ("threadId", saved.value.required("threadId")?.clone()),
                ("target", settings.required("target")?.clone()),
                ("model", settings.required("model")?.clone()),
                ("effort", settings.required("effort")?.clone()),
                ("threadCwd", settings.required("threadCwd")?.clone()),
                (
                    "developerInstructions",
                    plan.value.required("developerInstructions")?.clone(),
                ),
                ("retainedAt", IvyJson::String(date_to_iso(now as f64)?)),
                (
                    "expiresAt",
                    IvyJson::Number((now + 90 * 24 * 60 * 60 * 1000) as f64),
                ),
            ]),
        )?;
        Ok(())
    }
    fn retire(&self, work: &TriageDocument, plan: &TriageDocument) -> IvyResult<()> {
        self.native.detach_plan(plan)?;
        self.native.store().journal.complete_technical_workflow(
            &work.pin.object_id,
            self.native.engine.operation_clock.utc_millis(),
        )
    }
    pub async fn step(&self, item: &IvyJson) -> IvyResult<&'static str> {
        let work = self.native.prepare(item).await?;
        let plan = self.native.plan(&work)?;
        let results = TriageCompletion {
            native: &self.native,
            plan: &plan,
            retained: self.retained,
        };
        let operation_id = work.value.required("operationId")?;
        let engine = self.native.engine;
        let caller = engine
            .evidence
            .settings
            .required("identity")?
            .required("principalId")?;
        if work.value.get("phase") == Some(&"failed".into()) {
            return Ok("failed");
        }
        if work.value.get("phase") == Some(&"superseded".into()) {
            return Ok("superseded");
        }
        let current = engine.evidence.item(item.required("objectId")?).await?;
        let previous = engine.evidence.find(caller, operation_id).await?;
        if current
            .required("value")?
            .get("decision")
            .is_some_and(|value| !value.is_null())
            && previous.is_none()
        {
            if !self.native.release_closed(&plan).await? {
                return Ok("pending");
            }
            let mut next = work.value.clone();
            next.insert("phase", "superseded".into());
            next.insert("errorCode", "secretary_triage_item_changed".into());
            self.finish(&work, &next)?;
            self.retire(&work, &plan)?;
            return Ok("superseded");
        }
        let completion = match results.find()? {
            Some(completion) => completion,
            None => {
                evidence(previous.is_none() && work.value.get("phase") == Some(&"pending".into()))?;
                self.native.guard(&plan, false).await?;
                let Some(started) = self.native.begin_turn(&plan).await? else {
                    return Ok("pending");
                };
                let Some(completion) = results
                    .collect(&started.turn, &started.thread_id, &started.epoch)
                    .await?
                else {
                    return Ok("pending");
                };
                completion
            }
        };
        let request = IvyJson::object([
            ("action", "assess".into()),
            ("operationId", operation_id.clone()),
            (
                "expectedScope",
                plan.value.required("identity")?.required("scope")?.clone(),
            ),
            ("item", item.clone()),
            ("assessment", completion.value.required("result")?.clone()),
        ]);
        let accepted = engine.action(caller, &request).await?;
        if accepted.get("phase") == Some(&"failed".into())
            && accepted.get("errorCode") == Some(&"revision_conflict".into())
        {
            if !self.native.release_closed(&plan).await? {
                return Ok("pending");
            }
            let mut next = work.value.clone();
            next.insert("phase", "superseded".into());
            next.insert("completion", pin_value(&completion.pin));
            next.insert("errorCode", "secretary_triage_item_changed".into());
            self.finish(&work, &next)?;
            self.retire(&work, &plan)?;
            return Ok("superseded");
        }
        check(
            accepted.get("phase") == Some(&"succeeded".into())
                && accepted.get("effect").is_some_and(|value| !value.is_null())
                && same(accepted.required("request")?, &request)?,
            "secretary_triage_assessment_unconfirmed",
        )?;
        let assessed = engine.evidence.item(accepted.required("effect")?).await?;
        let decision = assessed.required("value")?.required("decision")?;
        evidence(
            decision.get("actor") == Some(caller)
                && decision.get("operationId") == Some(operation_id)
                && decision.get("assessment").is_some_and(|a| {
                    same(a, completion.value.required("result").unwrap()).unwrap_or(false)
                }),
        )?;
        self.retain_context(&work, &plan, &completion)?;
        let mut next = work.value.clone();
        next.insert("phase", "assessed".into());
        next.insert("completion", pin_value(&completion.pin));
        next.insert("assessment", accepted.required("effect")?.clone());
        next.insert("errorCode", IvyJson::Null);
        self.finish(&work, &next)?;
        self.retire(&work, &plan)?;
        Ok("assessed")
    }
    fn must_propagate(&self, error: &IvyError) -> bool {
        self.native
            .engine
            .evidence
            .cancellation
            .cancellation
            .is_cancelled()
            || error.code == "secretary_owner_mismatch"
    }
    async fn advance(&self, work: &TriageDocument) -> IvyResult<()> {
        let engine = self.native.engine;
        let key = format!("triage:{}", work.pin.object_id);
        engine.issues.clear(&format!(
            "triage:{}",
            text(work.value.required("item")?, "objectId")?.to_utf8()?
        ));
        match self.step(work.value.required("item")?).await {
            Ok(result) => {
                if result != "failed" {
                    engine.issues.clear(&key);
                }
            }
            Err(error) => {
                if self.must_propagate(&error) {
                    return Err(error);
                }
                let current = self.native.work(work.value.required("item")?)?;
                evidence(current.is_some())?;
                let current = current.unwrap();
                if current.value.get("phase") != Some(&"pending".into()) {
                    if current.value.get("phase") == Some(&"failed".into()) {
                        engine
                            .issues
                            .issue(&key, &text(&current.value, "errorCode")?.to_utf8()?);
                    } else {
                        engine.issues.clear(&key);
                    }
                    return Ok(());
                }
                if error.code == "secretary_triage_item_changed" {
                    engine.issues.clear(&key);
                } else {
                    if [
                        "secretary_triage_native_failed",
                        "secretary_triage_turn_failed",
                        "secretary_triage_result_invalid",
                        "secretary_triage_final_ambiguous",
                        "secretary_triage_unexpected_action",
                    ]
                    .contains(&error.code.as_str())
                    {
                        match self
                            .native
                            .release_closed(&self.native.plan(&current)?)
                            .await
                        {
                            Ok(false) => {
                                engine.issues.issue(&key, &error.code);
                                return Ok(());
                            }
                            Err(release) => {
                                if self.must_propagate(&release) {
                                    return Err(release);
                                }
                                engine.issues.issue(&key, &release.code);
                                return Ok(());
                            }
                            Ok(true) => (),
                        }
                        let plan = self.native.plan(&current)?;
                        let mut next = current.value.clone();
                        next.insert("phase", "failed".into());
                        next.insert("errorCode", error.code.clone().into());
                        self.finish(&current, &next)?;
                        self.retire(&current, &plan)?;
                    }
                    engine.issues.issue(&key, &error.code);
                }
            }
        }
        Ok(())
    }
    pub async fn tick(&self) -> IvyResult<()> {
        let active = self
            .active
            .borrow_mut()
            .get_or_insert_with(|| Arc::new(tokio::sync::OnceCell::new()))
            .clone();
        let result = active.get_or_init(|| self.run()).await.clone();
        if self
            .active
            .borrow()
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &active))
        {
            self.active.borrow_mut().take();
        }
        result
    }
    async fn run(&self) -> IvyResult<()> {
        let engine = self.native.engine;
        engine.evidence.verify_owner().await?;
        let work_rows = self.native.store().journal.list(R::Work.key(), true)?;
        let bound = engine
            .evidence
            .settings
            .required("recordsPerTick")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as usize;
        for row in work_rows
            .iter()
            .filter(|r| {
                ["failed", "superseded"]
                    .iter()
                    .any(|p| r.value.get("phase") == Some(&(*p).into()))
            })
            .take(bound)
        {
            let work = self
                .native
                .store()
                .read(R::Work, &pin_value(&row.pin), None, false)?;
            let checked = self.native.work(work.value.required("item")?)?;
            evidence(checked.as_ref().is_some_and(|w| {
                ["failed", "superseded"]
                    .iter()
                    .any(|p| w.value.get("phase") == Some(&(*p).into()))
            }))?;
            let checked = checked.unwrap();
            let key = format!("triage:{}", row.pin.object_id);
            if checked.value.get("phase") == Some(&"failed".into()) {
                engine
                    .issues
                    .issue(&key, &text(&checked.value, "errorCode")?.to_utf8()?);
            } else {
                engine.issues.clear(&key);
            }
            let key = format!("triage-release:{}", row.pin.object_id);
            let release = async {
                let plan = self.native.plan(&checked)?;
                if self.native.release_closed(&plan).await? {
                    self.retire(&checked, &plan)?;
                    engine.issues.clear(&key);
                } else {
                    engine
                        .issues
                        .issue(&key, "secretary_triage_release_pending");
                }
                Ok::<_, IvyError>(())
            }
            .await;
            if let Err(error) = release {
                if self.must_propagate(&error) {
                    return Err(error);
                }
                engine.issues.issue(&key, &error.code);
            }
        }
        for row in work_rows
            .iter()
            .filter(|r| r.value.get("phase") == Some(&"pending".into()))
            .take(bound)
        {
            self.advance(
                &self
                    .native
                    .store()
                    .read(R::Work, &pin_value(&row.pin), None, false)?,
            )
            .await?;
        }
        if work_rows
            .iter()
            .any(|r| r.value.get("phase") == Some(&"pending".into()))
        {
            return Ok(());
        }
        let mut query = IvyJson::object([
            ("contractKey", "secretary/item".into()),
            ("limit", IvyJson::Number(bound as f64)),
            (
                "orderBy",
                IvyJson::Array(vec![IvyJson::object([
                    ("field", "object.id".into()),
                    ("direction", "asc".into()),
                ])]),
            ),
            (
                "where",
                IvyJson::object([
                    ("op", "and".into()),
                    (
                        "args",
                        IvyJson::Array(vec![
                            IvyJson::object([
                                ("op", "eq".into()),
                                ("field", "object.parentId".into()),
                                ("value", IvyJson::String(engine.evidence.store.root.clone())),
                            ]),
                            IvyJson::object([
                                ("op", "isNull".into()),
                                ("field", "data:/decision".into()),
                            ]),
                        ]),
                    ),
                ]),
            ),
        ]);
        if let Some(cursor) = self
            .item_cursor
            .borrow()
            .as_ref()
            .filter(|v| !(v).is_null())
        {
            query.insert("cursor", cursor.clone());
        }
        let page = engine
            .evidence
            .store
            .client
            .request("objects.query", query, RequestOptions::default())
            .await?;
        *self.item_cursor.borrow_mut() = page.get("nextCursor").filter(|c| !c.is_null()).cloned();
        for row in page
            .required("items")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            let item = IvyJson::object([
                ("objectId", row.required("objectId")?.clone()),
                ("revision", row.required("revision")?.clone()),
            ]);
            if self.native.work(&item)?.is_some() {
                continue;
            }
            let outcome = async {
                let work = self.native.prepare(&item).await?;
                self.advance(&work).await?;
                Ok::<_, IvyError>(())
            }
            .await;
            if let Err(error) = outcome {
                if self.must_propagate(&error) {
                    return Err(error);
                }
                engine.issues.issue(
                    &format!("triage:{}", text(&item, "objectId")?.to_utf8()?),
                    &error.code,
                );
                if self.native.work(&item)?.is_some() {
                    break;
                }
            } else if self
                .native
                .work(&item)?
                .is_some_and(|w| w.value.get("phase") == Some(&"pending".into()))
            {
                break;
            }
        }
        Ok(())
    }
}
