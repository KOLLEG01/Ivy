//! Original account-bound native attachment reads and final coverage verification.
use crate::{
    inbox_evidence::truthy,
    media_schema::{MediaSchema, validate_media},
    media_spool::{SpoolJournal, Validation},
    media_store::{
        AdmissionFuture, MediaAdmissionGuard, MediaStore, verify_outlook_media_admission,
    },
    microsoft_schema::{MICROSOFT_METHODS, validate_microsoft},
    microsoft_store::{
        MicrosoftEpochBoundaries, MicrosoftGuard, MicrosoftGuardFuture, native_call,
    },
    outlook_media::{outlook_attachments, outlook_materialization},
    outlook_projection::{microsoft_mcp, microsoft_profile},
    outlook_source::OutlookSource,
    source_identity::catalogue_account_hash,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    agent_schema::validate_agent,
    hash::{digest, hash_json},
    json::{canonical, js_string},
    operation::derive_operation_id_js,
};
use ivy_sdk::{
    native_evidence::{native_instant, native_operation_terminal, validate_native_progress},
    native_operation::{
        NativeOperationJournal, NativeOperationOwner, NativeReconciliation,
        reconcile_native_operation_local,
    },
    native_owner::NativeOwner,
    operation_ids::new_operation_id,
    rpc::RequestOptions,
};
use std::{cell::RefCell, future::Future, pin::Pin};

fn check(condition: bool, code: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(
            code,
            "Native media needs its exact original source, account, call and selected attachment.",
        ))
    }
}
fn checked(condition: bool) -> IvyResult<()> {
    check(condition, "media_native_evidence_invalid")
}
fn same(a: &IvyJson, b: &IvyJson) -> IvyResult<bool> {
    Ok(hash_json(a)? == hash_json(b)?)
}
fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn instant(value: &IvyJson, key: &str) -> IvyResult<i64> {
    native_instant(&text(value, key)?, None)
}
fn object(value: Option<&IvyJson>) -> IvyResult<&IvyJson> {
    checked(value.is_some_and(|value| value.as_object().is_some()))?;
    Ok(value.unwrap())
}

/// The composition must supply the complete bounded domain downloader. There is
/// no default transport or alternate account/URL path at this boundary.
pub struct DownloadedMedia {
    pub bytes: Vec<u8>,
    pub evidence: IvyJson,
}
pub type DownloadFuture<'a> = Pin<Box<dyn Future<Output = IvyResult<DownloadedMedia>> + 'a>>;
pub trait OutlookMediaDownloader {
    fn download<'a>(
        &'a self,
        materialization: &'a IvyJson,
        cancellation: &'a RequestOptions,
    ) -> DownloadFuture<'a>;
}
pub struct MediaNativeValidation;
impl Validation for MediaNativeValidation {
    fn operation(&self, value: &IvyJson) -> IvyResult<()> {
        validate_agent("Operation", value)
    }
    fn progress(&self, previous: &IvyJson, next: &IvyJson) -> IvyResult<()> {
        validate_native_progress(previous, next, None)
    }
}
struct AttachmentResult {
    attachment: IvyJson,
    operation_id: Option<JsString>,
    error_code: Option<String>,
}
struct State {
    next: Option<usize>,
    thread_id: Option<JsString>,
    attachments: Vec<IvyJson>,
    gap: Option<String>,
    download: Option<JsString>,
    results: Vec<AttachmentResult>,
    observed_at: JsString,
}
pub struct OutlookMediaNative<'a, 'source, 'engine, 'store, 'journal, V> {
    pub source: &'a OutlookSource<'source, 'engine, 'store, 'journal>,
    pub spool: &'a RefCell<SpoolJournal<V>>,
    pub owner: NativeOwner,
    pub downloader: &'a dyn OutlookMediaDownloader,
}
impl<V: Validation> MediaAdmissionGuard for OutlookMediaNative<'_, '_, '_, '_, '_, V> {
    fn verify<'a>(&'a self, admission: &'a IvyJson, capture: &'a IvyJson) -> AdmissionFuture<'a> {
        Box::pin(verify_outlook_media_admission(
            self.source.native.engine,
            &self.source.native.configuration,
            admission,
            capture,
        ))
    }
}
impl<V: Validation> MicrosoftGuard for OutlookMediaNative<'_, '_, '_, '_, '_, V> {
    fn check(&self) -> MicrosoftGuardFuture<'_> {
        Box::pin(self.source.native.engine.evidence.verify_owner())
    }
}
impl<'a, 'source, 'engine, 'store, 'journal, V: Validation>
    OutlookMediaNative<'a, 'source, 'engine, 'store, 'journal, V>
{
    pub fn new(
        source: &'a OutlookSource<'source, 'engine, 'store, 'journal>,
        spool: &'a RefCell<SpoolJournal<V>>,
        downloader: &'a dyn OutlookMediaDownloader,
    ) -> Self {
        Self {
            source,
            spool,
            owner: source.native.owner.clone(),
            downloader,
        }
    }
    pub fn publication(&self) -> MediaStore<'_, 'engine, 'store, 'journal> {
        MediaStore {
            engine: self.source.native.engine,
            admission_guard: self,
        }
    }
    fn boundaries(&self) -> MicrosoftEpochBoundaries<'_> {
        MicrosoftEpochBoundaries {
            journal: self.source.native.engine.evidence.store.journal,
            owner: &self.owner,
            namespace: "secretary/media",
        }
    }
    pub async fn prepare(
        &self,
        admission: &IvyJson,
        item: &IvyJson,
        capture: &IvyJson,
    ) -> IvyResult<IvyJson> {
        self.verify(admission, capture).await?;
        let mut plan = self.spool.borrow().read_plan(item)?;
        if plan.is_none() {
            let status = self.owner.status().await?;
            let mut bindings = vec![];
            for method in MICROSOFT_METHODS {
                let binding = self.owner.binding(&method.into(), None).await?;
                bindings.push(IvyJson::object([
                    ("method", method.into()),
                    (
                        "definitionHash",
                        binding.value().required("definitionHash")?.clone(),
                    ),
                    ("outputSchema", IvyJson::Object(vec![])),
                ]));
            }
            check(
                self.owner.status().await?.get("epoch") == status.get("epoch"),
                "media_native_epoch_changed",
            )?;
            plan = Some(self.spool.borrow().prepare_read(&IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("admission", admission.clone()),
                ("item", item.clone()),
                ("captureOperationId", capture.clone()),
                ("configuration", self.source.native.configuration.clone()),
                ("epoch", status.required("epoch")?.clone()),
                (
                    "createdAt",
                    IvyJson::String(self.source.native.engine.now()?),
                ),
                ("bindings", IvyJson::Array(bindings)),
            ]))?);
        }
        let plan = plan.unwrap();
        self.validate_plan(&plan)?;
        self.verify(
            plan.required("admission")?,
            plan.required("captureOperationId")?,
        )
        .await?;
        Ok(plan)
    }
    fn validate_plan(&self, plan: &IvyJson) -> IvyResult<()> {
        validate_media(MediaSchema::ReadPlan, plan)?;
        validate_microsoft("OutlookCollector", plan.required("configuration")?)?;
        let bindings = plan
            .required("bindings")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        checked(
            same(
                plan.required("configuration")?,
                &self.source.native.configuration,
            )? && bindings.len() == MICROSOFT_METHODS.len()
                && bindings
                    .iter()
                    .zip(MICROSOFT_METHODS)
                    .all(|(binding, method)| binding.get("method") == Some(&method.into())),
        )?;
        instant(plan, "createdAt")?;
        checked(
            plan.required("admission")?.get("itemObjectId")
                == plan.required("item")?.get("objectId")
                && match self.spool.borrow().read_plan(plan.required("item")?)? {
                    Some(saved) => same(&saved, plan)?,
                    None => false,
                },
        )
    }
    async fn invocation(
        &self,
        plan: &IvyJson,
        slot: usize,
        state: &State,
        operation_id: Option<JsString>,
    ) -> IvyResult<IvyJson> {
        let (method, params) = match slot {
            0 | 37 => (
                "account/read",
                IvyJson::object([("refreshToken", false.into())]),
            ),
            1 => (
                "thread/start",
                IvyJson::object([
                    (
                        "cwd",
                        plan.required("configuration")?
                            .required("threadCwd")?
                            .clone(),
                    ),
                    ("ephemeral", true.into()),
                    ("permissions", ":danger-full-access".into()),
                    ("approvalPolicy", "never".into()),
                ]),
            ),
            _ => {
                checked(state.thread_id.is_some())?;
                let thread = IvyJson::String(state.thread_id.as_ref().unwrap().clone());
                if slot == 38 {
                    (
                        "thread/unsubscribe",
                        IvyJson::object([("threadId", thread)]),
                    )
                } else {
                    let (tool, arguments) = if slot == 3 {
                        (
                            "list_attachments",
                            IvyJson::object([
                                (
                                    "message_id",
                                    plan.required("admission")?
                                        .required("providerMessageId")?
                                        .clone(),
                                ),
                                ("content_mode", "metadata_only".into()),
                            ]),
                        )
                    } else if (4..=35).contains(&slot) {
                        let attachment = state.attachments.get(slot - 4);
                        checked(attachment.is_some())?;
                        (
                            "fetch_attachment",
                            IvyJson::object([
                                (
                                    "message_id",
                                    plan.required("admission")?
                                        .required("providerMessageId")?
                                        .clone(),
                                ),
                                (
                                    "attachment_id",
                                    attachment.unwrap().required("attachmentId")?.clone(),
                                ),
                            ]),
                        )
                    } else {
                        ("get_profile", IvyJson::Object(vec![]))
                    };
                    (
                        "mcpServer/tool/call",
                        IvyJson::object([
                            ("threadId", thread),
                            ("server", "codex_apps".into()),
                            ("tool", format!("microsoft_outlook_email.{tool}").into()),
                            ("arguments", arguments),
                        ]),
                    )
                }
            }
        };
        let operation_id = match operation_id {
            Some(id) => id,
            None => {
                new_operation_id(
                    self.source.native.engine.evidence.store.client.clone(),
                    None,
                )
                .await?
            }
        };
        let binding = plan
            .required("bindings")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .find(|binding| binding.get("method") == Some(&method.into()))
            .ok_or_else(IvyError::internal)?;
        Ok(IvyJson::object([
            ("planHash", hash_json(plan)?.into()),
            ("slot", IvyJson::Number(slot as f64)),
            ("method", method.into()),
            ("params", params),
            ("operationId", IvyJson::String(operation_id)),
            (
                "definitionHash",
                binding.required("definitionHash")?.clone(),
            ),
        ]))
    }
    fn evaluate(
        &self,
        plan: &IvyJson,
        state: &mut State,
        call: &IvyJson,
        operation: &IvyJson,
    ) -> IvyResult<()> {
        self.owner.check_operation(operation, &native_call(call)?)?;
        checked(native_operation_terminal(operation))?;
        let prepared = instant(call, "preparedAt")?;
        checked(instant(operation, "createdAt")? >= prepared)?;
        state.observed_at = text(operation, "updatedAt")?;
        let _content_hash = digest(canonical(operation, 4 * 1024 * 1024)?.as_bytes());
        let slot = call
            .required("slot")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as usize;
        let evaluated = (|| -> IvyResult<()> {
            let result = operation.get("reply").and_then(|reply| reply.get("result"));
            check(
                operation.get("phase") == Some(&"succeeded".into()) && result.is_some(),
                "media_native_failed",
            )?;
            check(
                slot == 38 || operation.get("epoch") == plan.get("epoch"),
                "media_native_epoch_changed",
            )?;
            let result = result.unwrap();
            match slot {
                0 | 37 => check(
                    Some(&catalogue_account_hash(result)?.into())
                        == plan.required("configuration")?.get("expectedAccountHash"),
                    "media_account_mismatch",
                )?,
                1 => {
                    let thread = object(object(Some(result))?.get("thread"))?
                        .get("id")
                        .and_then(IvyJson::as_js_string);
                    checked(thread.is_some_and(|value| !value.0.is_empty()))?;
                    state.thread_id = thread.cloned();
                }
                2 | 36 => check(
                    same(
                        &microsoft_profile(&microsoft_mcp(result)?)?,
                        plan.required("configuration")?.required("profile")?,
                    )?,
                    "media_account_mismatch",
                )?,
                3 => {
                    state.attachments = outlook_attachments(
                        &microsoft_mcp(result)?,
                        plan.required("admission")?.required("providerMessageId")?,
                    )?
                }
                38 => check(
                    object(Some(result))?.get("status").is_some_and(|value| {
                        matches!(
                            js_string(value).to_utf8().unwrap_or_default().as_str(),
                            "unsubscribed" | "notLoaded" | "notSubscribed"
                        )
                    }),
                    "media_cleanup_failed",
                )?,
                _ => {
                    let attachment = state
                        .attachments
                        .get(slot - 4)
                        .ok_or_else(IvyError::internal)?;
                    let materialization =
                        outlook_materialization(&microsoft_mcp(result)?, attachment)?;
                    let operation_id =
                        derive_operation_id_js(&text(call, "operationId")?, &"download".into())?;
                    let attempt = self.spool.borrow().prepare(
                        &IvyJson::object([
                            ("operationId", IvyJson::String(operation_id.clone())),
                            (
                                "captureOperationId",
                                plan.required("captureOperationId")?.clone(),
                            ),
                            ("item", plan.required("item")?.clone()),
                            ("admission", plan.required("admission")?.clone()),
                            ("materializationHash", hash_json(&materialization)?.into()),
                        ]),
                        &materialization,
                    )?;
                    if matches!(attempt.phase.as_str(), "prepared" | "downloading") {
                        state.download = Some(operation_id);
                        return Ok(());
                    }
                    state.results.push(AttachmentResult {
                        attachment: attachment.clone(),
                        operation_id: Some(operation_id),
                        error_code: attempt.error_code,
                    });
                }
            }
            state.next = if slot == 38 { None } else { Some(slot + 1) };
            Ok(())
        })();
        if let Err(error) = evaluated {
            if slot == 38 {
                return Err(error);
            }
            if (4..=35).contains(&slot) {
                state.results.push(AttachmentResult {
                    attachment: state
                        .attachments
                        .get(slot - 4)
                        .ok_or_else(IvyError::internal)?
                        .clone(),
                    operation_id: None,
                    error_code: Some(error.code),
                });
                state.next = Some(slot + 1);
            } else {
                state.gap.get_or_insert(error.code);
                state.next = state.thread_id.as_ref().map(|_| 38);
            }
        }
        Ok(())
    }
    async fn state(&self, plan: &IvyJson) -> IvyResult<State> {
        let mut state = State {
            next: Some(0),
            thread_id: None,
            attachments: vec![],
            gap: None,
            download: None,
            results: vec![],
            observed_at: text(plan, "createdAt")?,
        };
        self.verify(
            plan.required("admission")?,
            plan.required("captureOperationId")?,
        )
        .await?;
        let item = self
            .source
            .native
            .engine
            .evidence
            .item(plan.required("item")?.required("objectId")?)
            .await?;
        if text(
            item.required("value")?.required("message")?,
            "nativeRevision",
        )?
        .0
        .starts_with(&"outlook-inline-content:".encode_utf16().collect::<Vec<_>>())
        {
            state.next = None;
            state.gap = Some("media_inline_coverage_unavailable".into());
            return Ok(state);
        }
        while let Some(slot) = state.next {
            if (4..=35).contains(&slot) {
                let Some(attachment) = state.attachments.get(slot - 4) else {
                    state.next = Some(36);
                    continue;
                };
                if attachment.get("attachmentType") != Some(&"fileAttachment".into())
                    || !attachment.get("payloadFetchSupported").is_some_and(truthy)
                {
                    state.results.push(AttachmentResult {
                        attachment: attachment.clone(),
                        operation_id: None,
                        error_code: Some("media_attachment_unsupported".into()),
                    });
                    state.next = Some(slot + 1);
                    continue;
                }
            }
            // The original resolvedNativeCall is exactly this local read; it does not poll the owner.
            let Some(original) = self
                .spool
                .borrow()
                .native_call(&hash_json(plan)?, slot as i64)?
            else {
                break;
            };
            if let Some(boundary) = self.boundaries().read(
                plan.required("item")?,
                &original.call,
                plan.required("epoch")?,
            )? {
                checked(
                    original.seen.is_none()
                        && original.observation.is_none()
                        && slot != 38
                        && text(boundary.required("status")?, "observedAt")? >= state.observed_at,
                )?;
                state.observed_at = text(boundary.required("status")?, "observedAt")?;
                state.gap = Some("media_native_epoch_changed".into());
                state.next = state.thread_id.as_ref().map(|_| 38);
                continue;
            }
            let mut invocation = original.call.clone();
            invocation.remove("preparedAt");
            checked(same(
                &invocation,
                &self
                    .invocation(
                        plan,
                        slot,
                        &state,
                        Some(text(&original.call, "operationId")?),
                    )
                    .await?,
            )?)?;
            if let Some(seen) = &original.seen {
                self.owner
                    .check_operation(seen, &native_call(&original.call)?)?;
            }
            let Some(operation) = original.observation else {
                break;
            };
            self.evaluate(plan, &mut state, &original.call, &operation)?;
            if state.download.is_some() {
                break;
            }
        }
        Ok(state)
    }
    async fn acquire(&self, operation_id: &JsString, plan: &IvyJson) -> IvyResult<()> {
        let saved = self.spool.borrow().get(operation_id)?;
        check(saved.is_some(), "media_spool_intent_missing")?;
        if saved.unwrap().phase != "prepared" {
            return Ok(());
        }
        self.verify(
            plan.required("admission")?,
            plan.required("captureOperationId")?,
        )
        .await?;
        let cancellation = &self.source.native.engine.evidence.cancellation;
        if cancellation.cancellation.is_cancelled() {
            return Err(cancellation.cancel_error.clone());
        }
        if !self.spool.borrow_mut().begin_acquire(operation_id)? {
            return Ok(());
        }
        let result = async {
            let materialization = self.spool.borrow().materialization(operation_id)?;
            let downloaded = self
                .downloader
                .download(&materialization, cancellation)
                .await?;
            self.spool.borrow_mut().complete_acquire(
                operation_id,
                &downloaded.evidence,
                &downloaded.bytes,
            )
        }
        .await;
        if let Err(error) = result {
            self.spool
                .borrow_mut()
                .fail_acquire(operation_id, &error.code)?;
        }
        Ok(())
    }
    pub async fn verify_coverage(&self, plan: &IvyJson, coverage: &IvyJson) -> IvyResult<()> {
        self.validate_plan(plan)?;
        validate_media(MediaSchema::Coverage, coverage)?;
        let state = self.state(plan).await?;
        let entries = coverage
            .required("attachments")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        check(
            state.next.is_none()
                && state.download.is_none()
                && same(coverage.required("admission")?, plan.required("admission")?)?
                && coverage.get("observedAt") == Some(&IvyJson::String(state.observed_at.clone()))
                && coverage.get("gap")
                    == Some(
                        &state
                            .gap
                            .clone()
                            .map(IvyJson::from)
                            .unwrap_or(IvyJson::Null),
                    )
                && entries.len() == state.attachments.len(),
            "media_coverage_invalid",
        )?;
        for (index, attachment) in state.attachments.iter().enumerate() {
            let saved = &entries[index];
            let result = state.results.iter().find(|result| {
                result.attachment.get("attachmentId") == attachment.get("attachmentId")
            });
            let error = Self::attachment_error(&state, result);
            check(
                same(saved.required("attachment")?, attachment)?
                    && saved.get("errorCode")
                        == Some(&error.clone().map(IvyJson::from).unwrap_or(IvyJson::Null)),
                "media_coverage_invalid",
            )?;
            if error.is_some() {
                check(
                    saved.get("manifest") == Some(&IvyJson::Null),
                    "media_coverage_invalid",
                )?;
            } else {
                let operation_id = result.and_then(|result| result.operation_id.as_ref());
                let manifest = saved.get("manifest").filter(|value| truthy(value));
                check(
                    operation_id.is_some() && manifest.is_some(),
                    "media_coverage_invalid",
                )?;
                let attempt = self.spool.borrow().get(operation_id.unwrap())?;
                check(
                    attempt
                        .as_ref()
                        .is_some_and(|attempt| attempt.phase == "acquired"),
                    "media_coverage_invalid",
                )?;
                let manifest = self
                    .publication()
                    .manifest(manifest.unwrap(), Some(plan.required("admission")?))
                    .await?;
                check(
                    attempt.unwrap().evidence.as_ref().is_some_and(|evidence| {
                        same(
                            manifest.required("evidence").unwrap_or(&IvyJson::Null),
                            evidence,
                        )
                        .unwrap_or(false)
                    }),
                    "media_coverage_invalid",
                )?;
            }
        }
        Ok(())
    }
    fn attachment_error(state: &State, result: Option<&AttachmentResult>) -> Option<String> {
        state
            .gap
            .clone()
            .or_else(|| result.and_then(|result| result.error_code.clone()))
            .or_else(|| {
                if result.is_some_and(|result| result.operation_id.is_some()) {
                    None
                } else {
                    Some("media_acquisition_incomplete".into())
                }
            })
    }
    pub async fn step(&self, plan: &IvyJson) -> IvyResult<IvyJson> {
        self.validate_plan(plan)?;
        self.source.native.engine.evidence.verify_owner().await?;
        let mut state = self.state(plan).await?;
        if let Some(operation_id) = &state.download {
            self.acquire(operation_id, plan).await?;
            return Ok(IvyJson::object([("phase", "pending".into())]));
        }
        if let Some(slot) = state.next {
            let original = self
                .spool
                .borrow()
                .native_call(&hash_json(plan)?, slot as i64)?;
            let call = match &original {
                Some(original) => original.call.clone(),
                None => {
                    let mut call = self.invocation(plan, slot, &state, None).await?;
                    call.insert(
                        "preparedAt",
                        IvyJson::String(self.source.native.engine.now()?),
                    );
                    self.spool.borrow().prepare_native_call(&call)?.call
                }
            };
            let result = reconcile_native_operation_local(
                &self.owner,
                &native_call(&call)?,
                NativeOperationJournal {
                    current: (),
                    previous: original.as_ref().and_then(|original| {
                        original
                            .observation
                            .clone()
                            .or_else(|| original.seen.clone())
                    }),
                    was_observed: original.as_ref().is_some_and(|original| {
                        original.observation.is_some() || original.seen.is_some()
                    }),
                    retain: |operation| {
                        let result = self
                            .spool
                            .borrow()
                            .observe_native_call(&call, &operation)
                            .map(|_| ());
                        async move { result }
                    },
                },
                Some(
                    |invocation: ivy_sdk::native_operation::NativeOperationCall,
                     absence: IvyJson| async move {
                        if slot != 38 && absence.get("epoch") != plan.get("epoch") {
                            let original = IvyJson::object([
                                (
                                    "operationId",
                                    IvyJson::String(invocation.operation_id.clone()),
                                ),
                                ("method", IvyJson::String(invocation.method.clone())),
                                ("params", invocation.params.clone()),
                                (
                                    "definitionHash",
                                    IvyJson::String(invocation.definition_hash.clone()),
                                ),
                            ]);
                            self.boundaries()
                                .record(
                                    plan.required("item")?,
                                    &original,
                                    plan.required("epoch")?,
                                    &absence,
                                    self,
                                )
                                .await?;
                            return Ok(None);
                        }
                        self.owner
                            .dispatch_local(&invocation, || async {
                                self.source.native.engine.evidence.verify_owner().await?;
                                self.verify(
                                    plan.required("admission")?,
                                    plan.required("captureOperationId")?,
                                )
                                .await?;
                                let status = self.owner.status().await?;
                                check(
                                    status.get("epoch") == absence.get("epoch")
                                        && (slot == 38 || status.get("epoch") == plan.get("epoch")),
                                    "media_native_epoch_changed",
                                )
                            })
                            .await
                    },
                ),
                Some("media_native_absence_conflict"),
            )
            .await?;
            if matches!(result, NativeReconciliation::Absent { .. }) {
                return Ok(IvyJson::object([("phase", "pending".into())]));
            }
            state = self.state(plan).await?;
            if let Some(operation_id) = &state.download {
                self.acquire(operation_id, plan).await?;
            }
            return Ok(IvyJson::object([("phase", "pending".into())]));
        }
        let mut entries = vec![];
        for attachment in &state.attachments {
            let result = state.results.iter().find(|result| {
                result.attachment.get("attachmentId") == attachment.get("attachmentId")
            });
            let error = Self::attachment_error(&state, result);
            let manifest =
                if error.is_none() && result.is_some_and(|result| result.operation_id.is_some()) {
                    let (attempt, bytes) = self
                        .spool
                        .borrow()
                        .payload(result.unwrap().operation_id.as_ref().unwrap())?;
                    self.publication()
                        .publish_acquired(attempt, bytes)
                        .await?
                        .required("pin")?
                        .clone()
                } else {
                    IvyJson::Null
                };
            entries.push(IvyJson::object([
                ("attachment", attachment.clone()),
                ("manifest", manifest),
                (
                    "errorCode",
                    error.map(IvyJson::from).unwrap_or(IvyJson::Null),
                ),
            ]));
        }
        let coverage = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("admission", plan.required("admission")?.clone()),
            ("observedAt", IvyJson::String(state.observed_at)),
            ("gap", state.gap.map(IvyJson::from).unwrap_or(IvyJson::Null)),
            ("attachments", IvyJson::Array(entries)),
        ]);
        validate_media(MediaSchema::Coverage, &coverage)?;
        Ok(IvyJson::object([
            ("phase", "complete".into()),
            ("coverage", coverage),
        ]))
    }
}
