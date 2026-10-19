//! Publish one retained Outlook page through the original Secretary action journal.
use crate::{
    engine::SecretaryEngine,
    inbox_evidence::{identity_hash, ignored_reason, message_hash, source_identity, truthy},
    microsoft_schema::MicrosoftRecord,
    microsoft_store::{MicrosoftDocument, pin_value},
    outlook_native::{OutlookNative, outlook_name},
    store::{Record, mutation},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json};
use ivy_sdk::tools::ToolClients;
use std::sync::Arc;

fn check(condition: bool, code: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(
            code,
            "Outlook publication requires exact original Secretary captures and checkpoint.",
        ))
    }
}
fn same(a: &IvyJson, b: &IvyJson) -> IvyResult<bool> {
    Ok(hash_json(a)? == hash_json(b)?)
}
fn publication_check(condition: bool) -> IvyResult<()> {
    check(condition, "microsoft_publication_invalid")
}

pub struct OutlookSource<'a, 'engine, 'store, 'journal> {
    pub native: OutlookNative<'a, 'engine, 'store, 'journal>,
    not_before: Option<JsString>,
}
impl<'a, 'engine, 'store, 'journal> OutlookSource<'a, 'engine, 'store, 'journal> {
    pub fn new(
        engine: &'a SecretaryEngine<'engine, 'store, 'journal>,
        configuration: &IvyJson,
        tools: Arc<ToolClients>,
    ) -> IvyResult<Self> {
        Ok(Self {
            native: OutlookNative::new(engine, configuration, tools)?,
            not_before: None,
        })
    }
    async fn request_id(&self, proof: &MicrosoftDocument, scope: IvyJson) -> IvyResult<JsString> {
        let first = proof
            .value
            .required("calls")?
            .as_array()
            .and_then(|calls| calls.first());
        publication_check(first.is_some())?;
        let call = self.native.store().read(
            MicrosoftRecord::OutlookCall,
            first.unwrap().required("objectId")?,
            false,
            None,
        )?;
        mutation(&[call.value.required("operationId")?.clone(), scope])
    }
    async fn capture_request(&self, proof: &MicrosoftDocument, index: usize) -> IvyResult<IvyJson> {
        let message = proof
            .value
            .get("page")
            .and_then(|page| page.get("messages"))
            .and_then(IvyJson::as_array)
            .and_then(|messages| messages.get(index));
        publication_check(message.is_some())?;
        Ok(IvyJson::object([
            ("action", "capture".into()),
            (
                "operationId",
                IvyJson::String(
                    self.request_id(
                        proof,
                        IvyJson::Array(vec![
                            "outlook-capture".into(),
                            pin_value(&proof.pin),
                            IvyJson::Number(index as f64),
                        ]),
                    )
                    .await?,
                ),
            ),
            (
                "expectedScope",
                self.native
                    .engine
                    .evidence
                    .settings
                    .required("identity")?
                    .required("scope")?
                    .clone(),
            ),
            (
                "sourceId",
                self.native.configuration.required("sourceId")?.clone(),
            ),
            ("message", message.unwrap().clone()),
        ]))
    }
    async fn checkpoint_request(
        &self,
        proof: &MicrosoftDocument,
        plan: &MicrosoftDocument,
    ) -> IvyResult<IvyJson> {
        let page = proof.value.get("page").filter(|page| truthy(page));
        publication_check(page.is_some())?;
        let messages = page
            .unwrap()
            .required("messages")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        let mut captures = Vec::with_capacity(messages.len());
        for index in 0..messages.len() {
            captures.push(
                self.capture_request(proof, index)
                    .await?
                    .required("operationId")?
                    .clone(),
            );
        }
        Ok(IvyJson::object([
            ("action", "checkpoint".into()),
            (
                "operationId",
                IvyJson::String(
                    self.request_id(
                        proof,
                        IvyJson::Array(vec!["outlook-checkpoint".into(), pin_value(&proof.pin)]),
                    )
                    .await?,
                ),
            ),
            (
                "expectedScope",
                self.native
                    .engine
                    .evidence
                    .settings
                    .required("identity")?
                    .required("scope")?
                    .clone(),
            ),
            (
                "sourceId",
                self.native.configuration.required("sourceId")?.clone(),
            ),
            (
                "previousCursor",
                plan.value.required("previousCursor")?.clone(),
            ),
            (
                "nextCursor",
                format!("outlook-page:{}", &hash_json(&pin_value(&proof.pin))?[7..]).into(),
            ),
            ("captures", IvyJson::Array(captures)),
        ]))
    }
    async fn confirmed(&self, pin: &IvyJson, request: &IvyJson) -> IvyResult<IvyJson> {
        let evidence = self.native.engine.evidence;
        let operation = evidence.store.read(Record::Operation, pin).await?;
        let caller = evidence
            .settings
            .required("identity")?
            .required("principalId")?;
        let actual = evidence
            .find(caller, request.required("operationId")?)
            .await?;
        let value = operation.required("value")?;
        publication_check(match actual {
            Some(actual) => {
                same(actual.required("pin")?, pin)?
                    && value.get("callerPrincipalId") == Some(caller)
                    && value.get("requestHash") == Some(&hash_json(request)?.into())
                    && value.get("phase") == Some(&"succeeded".into())
            }
            None => false,
        })?;
        let definition = value.required("source")?.required("definition")?;
        let capture = request.get("action") == Some(&"capture".into());
        if capture && value.get("ignoredReason").is_some_and(truthy) {
            publication_check(
                !value.get("effect").is_some_and(truthy)
                    && !value.get("request").is_some_and(truthy)
                    && !value.get("prepared").is_some_and(truthy)
                    && ignored_reason(definition, request.required("message")?)?
                        .is_some_and(|reason| value.get("ignoredReason") == Some(&reason.into())),
            )?;
            return Ok(operation);
        }
        let effect = value
            .get("effect")
            .filter(|effect| truthy(effect))
            .ok_or_else(IvyError::internal)?;
        if capture {
            publication_check(!value.get("ignoredReason").is_some_and(truthy))?;
            let item = evidence.item(effect).await?;
            let item = item.required("value")?;
            publication_check(
                item.get("sourceId") == request.get("sourceId")
                    && item.get("identityHash")
                        == Some(
                            &identity_hash(
                                request.required("sourceId")?,
                                request.required("message")?,
                            )?
                            .into(),
                        )
                    && item.get("messageHash")
                        == Some(&message_hash(request.required("message")?)?.into()),
            )?;
        } else {
            let source = evidence.source(request.required("sourceId")?).await?;
            publication_check(
                source.required("pin")?.get("objectId") == effect.get("objectId")
                    && same(
                        &source_identity(source.required("value")?.required("definition")?)?,
                        &source_identity(definition)?,
                    )?,
            )?;
        }
        Ok(operation)
    }
    async fn publication(
        &self,
        proof: &MicrosoftDocument,
        plan: &MicrosoftDocument,
        create: bool,
    ) -> IvyResult<MicrosoftDocument> {
        publication_check(
            !proof.value.get("gap").is_some_and(truthy)
                && proof.value.get("page").is_some_and(truthy),
        )?;
        let store = self.native.store();
        let name = outlook_name("publication", &pin_value(&proof.pin))?;
        let publication = if create {
            Some(
                store
                    .create(
                        MicrosoftRecord::OutlookPublication,
                        &name,
                        &IvyJson::object([
                            ("schemaVersion", IvyJson::Number(1.0)),
                            ("evidence", pin_value(&proof.pin)),
                            ("captured", IvyJson::Array(vec![])),
                            ("checkpoint", IvyJson::Null),
                        ]),
                        false,
                    )
                    .await?,
            )
        } else {
            store.find(MicrosoftRecord::OutlookPublication, &name, false)?
        };
        publication_check(publication.is_some())?;
        let publication = publication.unwrap();
        let captured = publication
            .value
            .required("captured")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        let count = proof
            .value
            .required("page")?
            .required("messages")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .len();
        let checkpoint = publication.value.required("checkpoint")?;
        publication_check(
            same(
                publication.value.required("evidence")?,
                &pin_value(&proof.pin),
            )? && captured.len() <= count
                && publication.pin.revision
                    == (captured.len() + 1 + usize::from(truthy(checkpoint))) as i64,
        )?;
        for (index, pin) in captured.iter().enumerate() {
            self.confirmed(pin, &self.capture_request(proof, index).await?)
                .await?;
        }
        if truthy(checkpoint) {
            publication_check(captured.len() == count)?;
            self.confirmed(checkpoint, &self.checkpoint_request(proof, plan).await?)
                .await?;
        }
        Ok(publication)
    }
    pub async fn head(&self) -> IvyResult<MicrosoftDocument> {
        let head = self.native.raw_head().await?;
        if let Some(pending) = head.value.get("pending").filter(|value| truthy(value)) {
            let plan = self.native.plan(pending)?;
            publication_check(
                plan.value.required("head")?.get("objectId")
                    == Some(&head.pin.object_id.clone().into()),
            )?;
        } else if let Some(last) = head.value.get("lastPage").filter(|value| truthy(value)) {
            let proof = self.native.evidence(last).await?;
            let plan = self.native.plan(proof.value.required("plan")?)?;
            publication_check(
                plan.value.required("head")?.get("objectId")
                    == Some(&head.pin.object_id.clone().into()),
            )?;
            if !proof.value.get("gap").is_some_and(truthy) {
                publication_check(truthy(
                    self.publication(&proof, &plan, false)
                        .await?
                        .value
                        .required("checkpoint")?,
                ))?;
            }
        }
        Ok(head)
    }
    async fn publish(
        &self,
        proof: &MicrosoftDocument,
        plan: &MicrosoftDocument,
    ) -> IvyResult<bool> {
        let publication = self.publication(proof, plan, true).await?;
        if publication.value.get("checkpoint").is_some_and(truthy) {
            return Ok(true);
        }
        let captured = publication
            .value
            .required("captured")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        let count = proof
            .value
            .required("page")?
            .required("messages")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .len();
        let request = if captured.len() < count {
            self.capture_request(proof, captured.len()).await?
        } else {
            self.checkpoint_request(proof, plan).await?
        };
        let evidence = self.native.engine.evidence;
        let caller = evidence
            .settings
            .required("identity")?
            .required("principalId")?;
        let mut saved = evidence
            .find(caller, request.required("operationId")?)
            .await?;
        if saved
            .as_ref()
            .and_then(|row| row.get("value"))
            .and_then(|value| value.get("phase"))
            != Some(&"succeeded".into())
        {
            // An accepted checkpoint may already have committed its cursor. Its
            // original prepared revision and mutation govern recovery in Engine.
            if saved.is_none() {
                self.native.guard(plan).await?;
            }
            self.native.engine.action(caller, &request).await?;
            saved = evidence
                .find(caller, request.required("operationId")?)
                .await?;
        }
        check(
            saved
                .as_ref()
                .and_then(|row| row.get("value"))
                .and_then(|value| value.get("phase"))
                == Some(&"succeeded".into()),
            "microsoft_secretary_pending",
        )?;
        let saved = saved.unwrap();
        self.confirmed(saved.required("pin")?, &request).await?;
        let checkpoint = request.get("action") == Some(&"checkpoint".into());
        let mut value = publication.value.clone();
        if checkpoint {
            value.insert("checkpoint", saved.required("pin")?.clone());
        } else {
            let mut captured = captured.to_vec();
            captured.push(saved.required("pin")?.clone());
            value.insert("captured", IvyJson::Array(captured));
        }
        self.native
            .store()
            .amend(MicrosoftRecord::OutlookPublication, &publication, &value)
            .await?;
        Ok(checkpoint)
    }
    pub async fn step(&mut self) -> IvyResult<&'static str> {
        if let Some(at) = &self.not_before {
            if self.native.engine.now()? < *at {
                return Ok("idle");
            }
        }
        self.not_before = None;
        self.native.engine.evidence.verify_owner().await?;
        let mut head = self.head().await?;
        let Some(plan) = self.native.prepare(&head).await? else {
            self.not_before = Some(
                head.value
                    .get("nextPollAt")
                    .filter(|value| !value.is_null())
                    .unwrap_or(self.native.configuration.required("since")?)
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?
                    .clone(),
            );
            return Ok("idle");
        };
        let proof = match self.native.store().find(
            MicrosoftRecord::OutlookEvidence,
            &outlook_name("evidence", &pin_value(&plan.pin))?,
            true,
        )? {
            Some(proof) => Some(self.native.evidence(&pin_value(&proof.pin)).await?),
            None => self.native.collect(&plan).await?,
        };
        let Some(proof) = proof else {
            return Ok("pending");
        };
        let gap = proof.value.get("gap").is_some_and(truthy);
        if !gap && !self.publish(&proof, &plan).await? {
            return Ok("pending");
        }
        head = self.native.raw_head().await?;
        check(
            same(head.value.required("pending")?, &pin_value(&plan.pin))?,
            "microsoft_plan_changed",
        )?;
        let next = self.native.next_head(&head.value, &plan, &proof)?;
        self.native
            .store()
            .amend(MicrosoftRecord::OutlookHead, &head, &next)
            .await?;
        self.not_before = if next.get("continuation").is_some_and(truthy) {
            None
        } else {
            next.required("nextPollAt")?.as_js_string().cloned()
        };
        Ok(if gap {
            "gap"
        } else if proof.value.required("page")?.get("complete") == Some(&true.into()) {
            "complete"
        } else {
            "page"
        })
    }
}
