//! Replay admitted inbox work using original effects before authorizing new writes.
use crate::{
    inbox_evidence::InboxEvidence,
    store::{Record, conflict, mutation},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, hash::hash_json};
use std::{borrow::BorrowMut, cell::RefCell};

/// Synchronous access keeps foreground and preflight diagnostics ordered without
/// retaining an exclusive borrow across an RPC wait.
pub(crate) trait RecoveryIssues {
    fn issue(&self, key: &str, code: &str);
    fn clear(&self, key: &str);
}
impl<T: BorrowMut<RecoveryScan>> RecoveryIssues for RefCell<T> {
    fn issue(&self, key: &str, code: &str) {
        T::borrow_mut(&mut *self.borrow_mut()).issue(key, code);
    }
    fn clear(&self, key: &str) {
        T::borrow_mut(&mut *self.borrow_mut())
            .issues
            .retain(|(id, _)| id != key);
    }
}

fn need(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn same(a: &IvyJson, b: &IvyJson) -> IvyResult<bool> {
    Ok(hash_json(a)? == hash_json(b)?)
}
fn prepared_key(value: &IvyJson) -> IvyResult<Record> {
    match value.required("key")?.as_str()?.as_str() {
        "secretary/item" => Ok(Record::Item),
        "secretary/source" => Ok(Record::Source),
        _ => Err(IvyError::internal()),
    }
}
pub struct InboxRecovery<'a, 'engine, 'journal> {
    pub evidence: &'a InboxEvidence<'engine, 'journal>,
}
impl InboxRecovery<'_, '_, '_> {
    pub async fn capture_duplicate(
        &self,
        existing: &IvyJson,
        proposed: &IvyJson,
    ) -> IvyResult<IvyJson> {
        let item = self
            .evidence
            .item(existing.required("pin")?.required("objectId")?)
            .await?;
        let value = item.required("value")?;
        need(
            value.get("sourceId") == proposed.get("sourceId")
                && value.get("identityHash") == proposed.get("identityHash")
                && value.get("messageHash") == proposed.get("messageHash"),
            "secretary_identity_conflict",
            "The same native message revision has different original content.",
        )?;
        Ok(item.required("pin")?.clone())
    }
    pub async fn finish(&self, operation: &IvyJson, next: &IvyJson) -> IvyResult<IvyJson> {
        match self
            .evidence
            .store
            .amend(operation, Record::Operation, next)
            .await
        {
            Ok(saved) => Ok(saved),
            Err(error) => {
                let latest = self
                    .evidence
                    .store
                    .read(
                        Record::Operation,
                        operation.required("pin")?.required("objectId")?,
                    )
                    .await?;
                let saved = self.evidence.checked(&latest).await?;
                let value = saved.required("value")?;
                if value.get("phase") != Some(&"accepted".into()) {
                    need(
                        value.get("phase") == next.get("phase")
                            && same(value.required("effect")?, next.required("effect")?)?,
                        "secretary_evidence_conflict",
                        "The retained action outcome differs from its original effect.",
                    )?;
                    Ok(saved)
                } else {
                    Err(error)
                }
            }
        }
    }
    fn outcome(value: &IvyJson, phase: &str, effect: IvyJson, error: IvyJson) -> IvyJson {
        let mut next = value.clone();
        next.insert("phase", phase.into());
        next.insert("effect", effect);
        next.insert("errorCode", error);
        next
    }
    pub async fn apply(&self, operation: &IvyJson) -> IvyResult<IvyJson> {
        self.evidence.verify_owner().await?;
        let current = self
            .evidence
            .store
            .read(
                Record::Operation,
                operation.required("pin")?.required("objectId")?,
            )
            .await?;
        let operation = self.evidence.checked(&current).await?;
        let value = operation.required("value")?;
        if value.get("phase") != Some(&"accepted".into()) {
            return Ok(operation);
        }
        let prepared = value.get("prepared").filter(|v| !v.is_null());
        let mut effect = value.required("existingEffect")?.clone();
        if let Some(prepared) = prepared {
            let key = prepared_key(prepared)?;
            let destination = prepared.required("destination")?;
            let retained = if let Some(name) = destination.get("createName") {
                self.evidence
                    .store
                    .named(key, name.as_js_string().ok_or_else(IvyError::internal)?)
                    .await
            } else {
                let pin = destination.required("object")?;
                self.evidence
                    .store
                    .read(
                        key,
                        &IvyJson::object([
                            ("objectId", pin.required("objectId")?.clone()),
                            (
                                "revision",
                                IvyJson::Number(
                                    pin.required("revision")?
                                        .as_f64()
                                        .ok_or_else(IvyError::internal)?
                                        + 1.0,
                                ),
                            ),
                        ]),
                    )
                    .await
                    .map(Some)
            };
            let retained = match retained {
                Ok(v) => v,
                Err(e) if e.code == "not_found" => None,
                Err(e) => return Err(e),
            };
            if let Some(retained) = retained {
                if key == Record::Item && destination.get("createName").is_some() {
                    effect = self
                        .capture_duplicate(&retained, prepared.required("value")?)
                        .await?;
                } else if same(retained.required("value")?, prepared.required("value")?)? {
                    effect = retained.required("pin")?.clone();
                }
            }
            if !effect.is_null() {
                return self
                    .finish(
                        &operation,
                        &Self::outcome(value, "succeeded", effect, IvyJson::Null),
                    )
                    .await;
            }
            let source = self
                .evidence
                .settings
                .required("sources")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .find(|s| s.get("sourceId") == value.get("sourceId"));
            let authorized = match source {
                Some(source) => {
                    value.get("action") == Some(&"assess".into())
                        || source
                            .required("producerPrincipalIds")?
                            .as_array()
                            .ok_or_else(IvyError::internal)?
                            .contains(value.required("callerPrincipalId")?)
                }
                None => false,
            };
            need(
                authorized,
                "secretary_authorization_changed",
                "Original pending producer work is retained until its source identity is currently authorized.",
            )?;
            if key == Record::Item {
                need(
                    crate::inbox_evidence::ignored_reason(
                        source.unwrap(),
                        prepared.required("value")?.required("message")?,
                    )?
                    .is_none(),
                    "secretary_authorization_changed",
                    "The current source policy no longer admits a new effect for this message.",
                )?;
            }
        }
        let result = async {
            if let Some(prepared) = prepared {
                let key = prepared_key(prepared)?;
                let destination = prepared.required("destination")?;
                let id = mutation(&[
                    value.required("callerPrincipalId")?.clone(),
                    value.required("operationId")?.clone(),
                    "effect".into(),
                ])?;
                effect = match self
                    .evidence
                    .store
                    .write(key, prepared.required("value")?, destination, &id)
                    .await
                {
                    Ok(pin) => pin,
                    Err(error) => {
                        if value.get("action") != Some(&"capture".into())
                            || key != Record::Item
                            || destination.get("createName").is_none()
                        {
                            return Err(error);
                        }
                        let name = destination
                            .required("createName")?
                            .as_js_string()
                            .ok_or_else(IvyError::internal)?;
                        let duplicate = self.evidence.store.named(Record::Item, name).await?;
                        match duplicate {
                            Some(duplicate) => {
                                self.capture_duplicate(&duplicate, prepared.required("value")?)
                                    .await?
                            }
                            None => return Err(error),
                        }
                    }
                };
            }
            self.finish(
                &operation,
                &Self::outcome(value, "succeeded", effect, IvyJson::Null),
            )
            .await
        }
        .await;
        match result {
            Err(error) if conflict(&error) || error.code == "secretary_identity_conflict" => {
                self.finish(
                    &operation,
                    &Self::outcome(value, "failed", IvyJson::Null, error.code.into()),
                )
                .await
            }
            other => other,
        }
    }
}

/// Independent bounded recovery scan; preserves Map insertion order for diagnostics.
#[derive(Default)]
pub struct RecoveryScan {
    pub cursor: Option<String>,
    pub issues: Vec<(String, String)>,
}
impl RecoveryScan {
    pub fn issue(&mut self, object_id: &str, code: &str) {
        if let Some(entry) = self.issues.iter_mut().find(|(id, _)| id == object_id) {
            entry.1 = code.into();
        } else if self.issues.len() < 31 {
            self.issues.push((object_id.into(), code.into()));
        } else if let Some(entry) = self
            .issues
            .iter_mut()
            .find(|(id, _)| id == "recovery-overflow")
        {
            entry.1 = "secretary_recovery_issues_overflow".into();
        } else {
            self.issues.push((
                "recovery-overflow".into(),
                "secretary_recovery_issues_overflow".into(),
            ));
        }
    }
    pub async fn tick(
        &mut self,
        recovery: &InboxRecovery<'_, '_, '_>,
        limit: usize,
    ) -> IvyResult<()> {
        recovery.evidence.verify_owner().await?;
        let ids = self.page(recovery, limit)?;
        Self::recover_page(recovery, ids, &RefCell::new(self)).await
    }
    /// The engine and its independent workers share issues, never an exclusive
    /// RefCell borrow across RPC waits. The cursor advances before row recovery.
    pub(crate) async fn tick_shared(
        state: &RefCell<Self>,
        recovery: &InboxRecovery<'_, '_, '_>,
        limit: usize,
    ) -> IvyResult<()> {
        recovery.evidence.verify_owner().await?;
        let ids = state.borrow_mut().page(recovery, limit)?;
        Self::recover_page(recovery, ids, state).await
    }
    fn page(
        &mut self,
        recovery: &InboxRecovery<'_, '_, '_>,
        limit: usize,
    ) -> IvyResult<Vec<String>> {
        let ids = recovery
            .evidence
            .store
            .journal
            .accepted_ids(limit, self.cursor.as_deref().unwrap_or(""))?;
        self.cursor = if ids.len() < limit {
            None
        } else {
            ids.last().cloned()
        };
        Ok(ids)
    }
    async fn recover_page(
        recovery: &InboxRecovery<'_, '_, '_>,
        ids: Vec<String>,
        issues: &dyn RecoveryIssues,
    ) -> IvyResult<()> {
        for id in ids {
            let result = async {
                let operation = recovery
                    .evidence
                    .store
                    .read(Record::Operation, &id.clone().into())
                    .await?;
                recovery.apply(&operation).await
            }
            .await;
            match result {
                Ok(_) => issues.clear(&id),
                Err(error) => {
                    if recovery.evidence.cancellation.cancellation.is_cancelled() {
                        return Err(error);
                    }
                    issues.issue(&id, &error.code);
                }
            }
        }
        Ok(())
    }
}
