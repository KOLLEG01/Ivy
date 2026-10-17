//! Ordered durable outbox. All SQLite commands complete before awaiting Hive.
//! One mutable publisher belongs to the engine's retained publication future;
//! its request cancellation must be signalled before that future is joined.
use crate::{model, policy, store_owner::StoreOwner};
use ivy_contracts::{IvyError, IvyJson, IvyResult, Outcome, json::canonical};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use tokio_util::sync::CancellationToken;

/// Supplied by the shared live connection's scoped-operation cache. Collector
/// only supplies its existing scope labels; it does not own epoch/ID policy.
pub trait MutationIds: Send + Sync {
    fn scoped<'a>(&'a self, scope: IvyJson) -> RequestFuture<'a>;
}

pub struct Publisher {
    pub store: StoreOwner,
    pub root_id: String,
    pub maximum_retention: IvyJson,
}

impl Publisher {
    pub fn new(store: StoreOwner, root_id: String, maximum_retention: IvyJson) -> Self {
        Self {
            store,
            root_id,
            maximum_retention,
        }
    }

    pub async fn attach(
        &mut self,
        client: &dyn RpcClient,
        ids: &dyn MutationIds,
        configured_root: Option<String>,
        root_name: &str,
        options: RequestOptions,
    ) -> IvyResult<()> {
        let saved = match configured_root {
            Some(id) => Some(id),
            None => self.store.call(|s| s.metadata("rootId")).await?,
        };
        if let Some(id) = saved.filter(|s| !s.is_empty()) {
            let root = client
                .request("objects.read", object_id(&id), options)
                .await?;
            let object = model::field(&root, "object")?;
            require(
                object.get("contractKey") == Some(&"data-collector/root".into())
                    && !model::truth(object, "effectivelyArchived"),
                "Configured collector root must be an active data-collector/root object.",
            )?;
            self.root_id = id;
            return Ok(());
        }
        let found = client
            .request(
                "objects.query",
                IvyJson::object([
                    ("contractKey", "data-collector/root".into()),
                    ("where", equals("object.name", root_name.into())),
                    ("limit", IvyJson::Number(2.0)),
                ]),
                options.clone(),
            )
            .await?;
        let items = array(&found, "items")?;
        require(
            items.len() <= 1,
            "Collector root name is ambiguous. Configure rootObjectId.",
        )?;
        self.root_id = if let Some(item) = items.first() {
            model::text(item, "objectId")?
        } else {
            let written = client
                .request(
                    "objects.write",
                    IvyJson::object([
                        ("mutationId", mutation(ids, "root").await?),
                        (
                            "create",
                            IvyJson::object([
                                ("parentId", IvyJson::Null),
                                ("ownerObjectId", IvyJson::Null),
                                ("name", root_name.into()),
                                ("contractKey", "data-collector/root".into()),
                            ]),
                        ),
                        ("contractVersion", "1.0.0".into()),
                        ("references", model::object()),
                        (
                            "content",
                            content(IvyJson::object([("schemaVersion", IvyJson::Number(1.0))])),
                        ),
                    ]),
                    options,
                )
                .await?;
            model::text(model::field(&written, "object")?, "id")?
        };
        let id = self.root_id.clone();
        self.store
            .call(move |s| s.set_metadata("rootId", &id))
            .await
    }

    /// Stop the pass on every failure, including a definitive rejection. A later
    /// pass may advance to the next row, preserving the legacy order boundary.
    pub async fn flush(
        &mut self,
        client: &dyn RpcClient,
        ids: &dyn MutationIds,
        options: RequestOptions,
        closing: &CancellationToken,
    ) -> IvyResult<()> {
        let pending = self.store.call(|s| s.runs("publishing")).await?;
        for mut run in pending {
            if closing.is_cancelled() || options.cancellation.is_cancelled() {
                break;
            }
            if let Err(error) = self.publish(&mut run, client, ids, options.clone()).await {
                if error.outcome == Outcome::NotExecuted
                    && matches!(
                        error.code.as_str(),
                        "content_too_large" | "invalid_arguments" | "target_conflict"
                    )
                {
                    run.insert("status", "failed".into());
                    run.insert("error", IvyJson::String(error.code_js()));
                } else {
                    let mut text: Vec<u16> =
                        "Result publication pending: ".encode_utf16().collect();
                    text.extend(error.code_js().0);
                    run.insert("error", IvyJson::String(ivy_contracts::JsString(text)));
                }
                self.store.call(move |s| s.save_run(&run)).await?;
                break;
            }
        }
        Ok(())
    }

    async fn task(&self, id: &str) -> IvyResult<IvyJson> {
        let id = id.to_owned();
        self.store
            .call(move |s| {
                s.get(&id, false)?
                    .ok_or_else(|| IvyError::new("not_found", "Collector task not found."))
            })
            .await
    }

    async fn publish(
        &self,
        run: &mut IvyJson,
        client: &dyn RpcClient,
        ids: &dyn MutationIds,
        options: RequestOptions,
    ) -> IvyResult<()> {
        // Revalidate recovered event envelopes before querying or writing Hive.
        let event_count = model::field(run, "output")?
            .get("events")
            .map(|v| v.as_array().ok_or_else(invalid))
            .transpose()?
            .unwrap_or(&[])
            .len();
        for index in 0..event_count {
            canonical(&policy::event_content(run, index)?, policy::EVENT_BYTES)?;
        }
        let task_id = model::text(run, "taskId")?;
        let row = self.task(&task_id).await?;
        let root =
            model::optional_text(&row, "resultRootId").unwrap_or_else(|| self.root_id.clone());
        let mut binding = model::optional_text(&row, "resultObjectId").filter(|s| !s.is_empty());
        if binding.is_none() {
            let found = client
                .request(
                    "objects.query",
                    IvyJson::object([
                        ("contractKey", "data-collector/result".into()),
                        (
                            "where",
                            IvyJson::object([
                                ("op", "and".into()),
                                (
                                    "args",
                                    IvyJson::Array(vec![
                                        equals("object.parentId", root.clone().into()),
                                        equals("object.name", task_id.clone().into()),
                                    ]),
                                ),
                            ]),
                        ),
                        ("limit", IvyJson::Number(1.0)),
                    ]),
                    options.clone(),
                )
                .await?;
            binding = array(&found, "items")?
                .first()
                .map(|v| model::text(v, "objectId"))
                .transpose()?;
        }
        let current = if let Some(id) = &binding {
            Some(
                client
                    .request("objects.read", object_id(id), options.clone())
                    .await?,
            )
        } else {
            None
        };
        if let Some(value) = &current {
            let object = model::field(value, "object")?;
            require(
                object.get("parentId") == Some(&root.clone().into())
                    && object.get("contractKey") == Some(&"data-collector/result".into())
                    && !model::truth(object, "effectivelyArchived"),
                "Result object no longer belongs to this collector root.",
            )?;
        }
        let previous = current
            .as_ref()
            .and_then(|v| v.get("content"))
            .filter(|v| v.get("encoding") == Some(&"json".into()))
            .and_then(|v| v.get("value"));
        if previous.and_then(|v| v.get("runId")) != run.get("id") {
            // Run identities may contain UTF-16 strings which cannot be a Rust str.
            let mut label: Vec<u16> = "result:".encode_utf16().collect();
            label.extend(
                model::field(run, "id")?
                    .as_js_string()
                    .ok_or_else(invalid)?
                    .0
                    .iter(),
            );
            let mut params = IvyJson::object([
                ("mutationId", mutation_js(ids, label).await?),
                ("contractVersion", "1.0.0".into()),
                ("references", model::object()),
            ]);
            if let Some(value) = &current {
                params.insert(
                    "objectId",
                    model::field(model::field(value, "object")?, "id")?.clone(),
                );
                params.insert(
                    "expectedRevision",
                    model::field(model::field(value, "revision")?, "revision")?.clone(),
                );
            } else {
                params.insert(
                    "create",
                    IvyJson::object([
                        ("parentId", root.into()),
                        ("ownerObjectId", IvyJson::Null),
                        ("name", task_id.clone().into()),
                        ("contractKey", "data-collector/result".into()),
                    ]),
                );
            }
            params.insert("content", content(policy::result_content(run)?));
            let written = client
                .request("objects.write", params, options.clone())
                .await?;
            binding = Some(model::text(model::field(&written, "object")?, "id")?);
        }
        let id = task_id.clone();
        self.store
            .call(move |s| {
                let mut latest = s
                    .get(&id, false)?
                    .ok_or_else(|| IvyError::new("not_found", "Collector task not found."))?;
                latest.insert(
                    "resultObjectId",
                    binding.map(IvyJson::from).unwrap_or(IvyJson::Null),
                );
                s.save(&latest)
            })
            .await?;
        let mut index = model::number(run, "eventIndex")? as usize;
        while index < event_count {
            let mut label: Vec<u16> = "event:".encode_utf16().collect();
            label.extend(
                model::field(run, "id")?
                    .as_js_string()
                    .ok_or_else(invalid)?
                    .0
                    .iter(),
            );
            label.extend(format!(":{index}").encode_utf16());
            client
                .request(
                    "events.publish",
                    IvyJson::object([
                        ("topic", "data-collector.event".into()),
                        ("topicVersion", "1.0.0".into()),
                        ("mutationId", mutation_js(ids, label).await?),
                        ("payload", policy::event_content(run, index)?),
                    ]),
                    options.clone(),
                )
                .await?;
            index += 1;
            run.insert("eventIndex", IvyJson::Number(index as f64));
            let saved = run.clone();
            self.store.call(move |s| s.save_run(&saved)).await?;
        }
        let saved = run.clone();
        let id = task_id.clone();
        self.store
            .call(move |s| {
                let row = s
                    .get(&id, false)?
                    .ok_or_else(|| IvyError::new("not_found", "Collector task not found."))?;
                let mut saved = saved;
                saved.insert("status", "succeeded".into());
                saved.insert("error", IvyJson::Null);
                s.transaction(|s| {
                    s.save(&row)?;
                    s.save_run(&saved)
                })
            })
            .await?;
        self.retain(&task_id, client, ids, options).await
    }

    /// Used after publication and by the engine's fifteen-minute maintenance pass,
    /// including disabled/deleted rows. A retention failure never reruns work.
    pub async fn retain(
        &self,
        task_id: &str,
        client: &dyn RpcClient,
        ids: &dyn MutationIds,
        options: RequestOptions,
    ) -> IvyResult<()> {
        let id = task_id.to_owned();
        let row = self
            .store
            .call(move |s| {
                s.get(&id, true)?
                    .ok_or_else(|| IvyError::new("not_found", "Collector task not found."))
            })
            .await?;
        let Some(binding) = model::optional_text(&row, "resultObjectId").filter(|s| !s.is_empty())
        else {
            return Ok(());
        };
        let retention = policy::effective_retention(
            model::field(model::field(&row, "task")?, "retention")?,
            &self.maximum_retention,
        )?;
        let result: IvyResult<Option<String>> = async {
            let object = client
                .request("objects.stat", object_id(&binding), options.clone())
                .await?;
            let mut params = IvyJson::object([
                ("objectId", binding.into()),
                (
                    "expectedRevision",
                    model::field(&object, "currentRevision")?.clone(),
                ),
                (
                    "mutationId",
                    mutation(ids, &format!("prune:{}", uuid::Uuid::new_v4())).await?,
                ),
            ]);
            for (key, value) in retention.as_object().ok_or_else(invalid)? {
                params.insert(key.clone(), value.clone());
            }
            let pruned = client
                .request("objects.pruneRevisions", params, options)
                .await?;
            Ok(if model::number(&pruned, "protected")? != 0.0 {
                Some("Referenced revisions are protected from retention.".to_owned())
            } else if model::number(&pruned, "remainingBytes")?
                > model::number(&retention, "maximumBytes")?
            {
                Some(
                    "Retained history exceeds the byte budget; cleanup may still be pending."
                        .to_owned(),
                )
            } else {
                None
            })
        }
        .await;
        let warning = match result {
            Ok(value) => value.map(IvyJson::from).unwrap_or(IvyJson::Null),
            Err(error) => {
                let mut text: Vec<u16> = "Retention pending: ".encode_utf16().collect();
                text.extend(error.code_js().0);
                IvyJson::String(ivy_contracts::JsString(text))
            }
        };
        let id = task_id.to_owned();
        self.store
            .call(move |s| {
                let mut latest = s
                    .get(&id, true)?
                    .ok_or_else(|| IvyError::new("not_found", "Collector task not found."))?;
                latest.insert("retentionWarning", warning);
                s.save(&latest)
            })
            .await
    }
}

fn array<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a [IvyJson]> {
    model::field(value, key)?.as_array().ok_or_else(invalid)
}
fn invalid() -> IvyError {
    IvyError::new("invalid_arguments", "Invalid Collector publication data.")
}
fn require(ok: bool, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new("target_conflict", message))
    }
}
fn object_id(id: &str) -> IvyJson {
    IvyJson::object([("objectId", id.into())])
}
fn equals(field: &str, value: IvyJson) -> IvyJson {
    IvyJson::object([
        ("op", "eq".into()),
        ("field", field.into()),
        ("value", value),
    ])
}
fn content(value: IvyJson) -> IvyJson {
    IvyJson::object([("encoding", "json".into()), ("value", value)])
}
async fn mutation(ids: &dyn MutationIds, label: &str) -> IvyResult<IvyJson> {
    mutation_js(ids, label.encode_utf16().collect()).await
}
async fn mutation_js(ids: &dyn MutationIds, label: Vec<u16>) -> IvyResult<IvyJson> {
    let mut scope: Vec<u16> = "collector:".encode_utf16().collect();
    scope.extend(label);
    ids.scoped(IvyJson::String(ivy_contracts::JsString(scope)))
        .await
}
