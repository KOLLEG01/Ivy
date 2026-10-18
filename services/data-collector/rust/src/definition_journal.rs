//! Durable control-write requests and exact pinned-successor reconciliation.
//! The definition owner serializes mutations; network waits never hold SQLite.
use crate::{model, store_owner::StoreOwner};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    json::{canonical, parse, stringify},
};
use ivy_sdk::{
    operation_ids::new_operation_id,
    rpc::{RequestOptions, RpcClient},
};
use std::{cmp::Ordering, sync::Arc};

pub type CompareNames = Arc<dyn Fn(&JsString, &JsString) -> Ordering + Send + Sync>;
pub struct DefinitionJournal {
    pub store: StoreOwner,
    pub client: Arc<dyn RpcClient>,
    catalog_id: Option<IvyJson>,
    compare_names: CompareNames,
}
pub fn content(document: &IvyJson) -> IvyResult<&IvyJson> {
    let value = model::field(document, "content")?;
    require(
        value.get("encoding") == Some(&"json".into()),
        "target_conflict",
        "Collector control data must be JSON.",
    )?;
    model::field(value, "value")
}
pub fn id(document: &IvyJson) -> IvyResult<&IvyJson> {
    model::field(model::field(document, "object")?, "id")
}
pub fn revision(document: &IvyJson) -> IvyResult<&IvyJson> {
    model::field(model::field(document, "revision")?, "revision")
}
pub fn pin(document: &IvyJson) -> IvyResult<IvyJson> {
    Ok(IvyJson::object([
        ("objectId", id(document)?.clone()),
        ("revision", revision(document)?.clone()),
    ]))
}
pub fn require(test: bool, code: &str, message: &str) -> IvyResult<()> {
    if test {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn key(prefix: &str, operation: &JsString, step: Option<&str>) -> String {
    // SQLite TEXT keys use Node's UTF-8 conversion, unlike embedded JSON values.
    let mut key = prefix.encode_utf16().collect::<Vec<_>>();
    key.extend_from_slice(&operation.0);
    if let Some(step) = step {
        key.push(58);
        key.extend(step.encode_utf16());
    }
    String::from_utf16_lossy(&key)
}
impl DefinitionJournal {
    pub fn new(store: StoreOwner, client: Arc<dyn RpcClient>, compare_names: CompareNames) -> Self {
        Self {
            store,
            client,
            catalog_id: None,
            compare_names,
        }
    }
    pub async fn metadata(&self, key: String) -> IvyResult<Option<String>> {
        self.store.call(move |s| s.metadata(&key)).await
    }
    pub async fn set_metadata(&self, key: String, value: String) -> IvyResult<()> {
        self.store.call(move |s| s.set_metadata(&key, &value)).await
    }
    pub async fn mutation(&self, operation: &JsString, step: &str) -> IvyResult<IvyJson> {
        let key = key("definition-operation:", operation, Some(step));
        if let Some(id) = self.metadata(key.clone()).await? {
            return Ok(id.into());
        }
        let id = new_operation_id(self.client.clone(), None).await?;
        self.set_metadata(key, String::from_utf16_lossy(&id.0))
            .await?;
        Ok(IvyJson::String(id))
    }
    pub async fn all(&self, kind: &str) -> IvyResult<Vec<IvyJson>> {
        let mut items = Vec::new();
        let mut cursor = None;
        loop {
            let mut params = IvyJson::object([
                ("contractKey", format!("data-collector/{kind}").into()),
                ("includeContent", true.into()),
                ("limit", IvyJson::Number(200.0)),
            ]);
            if let Some(value) = cursor {
                params.insert("cursor", value);
            }
            let page = self
                .client
                .request("objects.query", params, RequestOptions::default())
                .await?;
            for item in model::field(&page, "items")?
                .as_array()
                .ok_or_else(IvyError::internal)?
            {
                let document = item
                    .get("document")
                    .filter(|v| !v.is_null())
                    .ok_or_else(|| {
                        IvyError::new(
                            "invalid_frame",
                            "Collector query omitted its pinned document.",
                        )
                    })?;
                items.push(document.clone());
            }
            cursor = page
                .get("nextCursor")
                .filter(|v| v.as_js_string().is_some_and(|s| !s.0.is_empty()))
                .cloned();
            if cursor.is_none() {
                return Ok(items);
            }
        }
    }
    pub async fn document(
        &self,
        kind: &str,
        id: IvyJson,
        revision: Option<IvyJson>,
    ) -> IvyResult<IvyJson> {
        let mut params = IvyJson::object([("objectId", id)]);
        if let Some(revision) = revision.filter(|v| v.as_f64() != Some(0.0)) {
            params.insert("revision", revision);
        }
        let value = self
            .client
            .request("objects.read", params, RequestOptions::default())
            .await?;
        let object = model::field(&value, "object")?;
        require(
            object.get("contractKey") == Some(&format!("data-collector/{kind}").into())
                && !model::truth(object, "effectivelyArchived"),
            "target_conflict",
            "Unexpected or archived collector control object.",
        )?;
        Ok(value)
    }
    pub async fn catalog(&mut self) -> IvyResult<IvyJson> {
        if let Some(id) = &self.catalog_id {
            return Ok(id.clone());
        }
        let found = self.all("catalog").await?;
        let result = if found.is_empty() {
            let mutation = self.mutation(&"catalog".into(), "create").await?;
            let created = self
                .client
                .request(
                    "objects.write",
                    IvyJson::object([
                        ("mutationId", mutation),
                        (
                            "create",
                            IvyJson::object([
                                ("name", "Collector definitions".into()),
                                ("parentId", IvyJson::Null),
                                ("ownerObjectId", IvyJson::Null),
                                ("contractKey", "data-collector/catalog".into()),
                            ]),
                        ),
                        ("contractVersion", "1.0.0".into()),
                        ("references", model::object()),
                        (
                            "content",
                            IvyJson::object([
                                ("encoding", "json".into()),
                                (
                                    "value",
                                    IvyJson::object([("schemaVersion", IvyJson::Number(1.0))]),
                                ),
                            ]),
                        ),
                    ]),
                    RequestOptions::default(),
                )
                .await?;
            id(&created)?.clone()
        } else {
            let mut ids = found
                .iter()
                .map(|d| {
                    id(d)?
                        .as_js_string()
                        .cloned()
                        .ok_or_else(IvyError::internal)
                })
                .collect::<IvyResult<Vec<_>>>()?;
            ids.sort_by(|a, b| (self.compare_names)(a, b));
            IvyJson::String(ids.remove(0))
        };
        self.catalog_id = Some(result.clone());
        Ok(result)
    }
    pub async fn retained(&self, operation: &JsString, step: &str) -> IvyResult<bool> {
        Ok(self
            .metadata(key("definition-write:", operation, Some(step)))
            .await?
            .is_some_and(|s| !s.is_empty()))
    }
    pub async fn write(
        &mut self,
        kind: &str,
        value: IvyJson,
        operation: &JsString,
        step: &str,
        previous: Option<&IvyJson>,
        references: IvyJson,
    ) -> IvyResult<IvyJson> {
        canonical(&value, 1_048_576)?;
        let key = key("definition-write:", operation, Some(step));
        let retained = if kind == "observation" {
            None
        } else {
            self.metadata(key.clone()).await?.filter(|s| !s.is_empty())
        };
        let mut request = if let Some(retained) = &retained {
            parse(retained, usize::MAX)?
        } else {
            let mutation = if kind == "observation" {
                IvyJson::String(new_operation_id(self.client.clone(), None).await?)
            } else {
                self.mutation(operation, step).await?
            };
            let mut request = IvyJson::object([
                ("mutationId", mutation),
                ("contractVersion", "1.0.0".into()),
                ("references", references),
            ]);
            if let Some(previous) = previous {
                request.insert("objectId", id(previous)?.clone());
                request.insert("expectedRevision", revision(previous)?.clone());
            } else {
                request.insert(
                    "create",
                    IvyJson::object([
                        ("name", format!("{kind}-{}", uuid::Uuid::new_v4()).into()),
                        ("parentId", self.catalog().await?),
                        ("ownerObjectId", IvyJson::Null),
                        ("contractKey", format!("data-collector/{kind}").into()),
                    ]),
                );
            }
            request.insert(
                "content",
                IvyJson::object([("encoding", "json".into()), ("value", value)]),
            );
            request
        };
        if retained.is_none() && kind != "observation" {
            self.set_metadata(key.clone(), stringify(&request, usize::MAX)?)
                .await?;
        }
        if retained.is_some() {
            let observed = if let Some(create) = request.get("create") {
                let page = self
                    .client
                    .request(
                        "objects.query",
                        IvyJson::object([
                            ("contractKey", format!("data-collector/{kind}").into()),
                            (
                                "where",
                                IvyJson::object([
                                    ("op", "eq".into()),
                                    ("field", "object.name".into()),
                                    ("value", model::field(create, "name")?.clone()),
                                ]),
                            ),
                            ("includeContent", true.into()),
                            ("limit", IvyJson::Number(2.0)),
                        ]),
                        RequestOptions::default(),
                    )
                    .await?;
                let items = model::field(&page, "items")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?;
                require(
                    items.len() <= 1,
                    "target_conflict",
                    "Ambiguous collector operation result.",
                )?;
                if let Some(item) = items.first() {
                    Some(
                        self.document(
                            kind,
                            model::field(item, "objectId")?.clone(),
                            Some(IvyJson::Number(1.0)),
                        )
                        .await?,
                    )
                } else {
                    None
                }
            } else {
                match self
                    .document(
                        kind,
                        model::field(&request, "objectId")?.clone(),
                        Some(IvyJson::Number(
                            model::number(&request, "expectedRevision")? + 1.0,
                        )),
                    )
                    .await
                {
                    Ok(document) => Some(document),
                    Err(error)
                        if matches!(error.code.as_str(), "not_found" | "revision_not_found") =>
                    {
                        None
                    }
                    Err(error) => return Err(error),
                }
            };
            if let Some(observed) = observed {
                if canonical(content(&observed)?, usize::MAX)?
                    == canonical(content(&request)?, usize::MAX)?
                {
                    return Ok(observed);
                }
            }
            request.insert(
                "mutationId",
                IvyJson::String(new_operation_id(self.client.clone(), None).await?),
            );
            self.set_metadata(key, stringify(&request, usize::MAX)?)
                .await?;
        }
        let written = self
            .client
            .request("objects.write", request, RequestOptions::default())
            .await?;
        self.document(
            kind,
            id(&written)?.clone(),
            Some(revision(&written)?.clone()),
        )
        .await
    }
    /// Guard the exact caller/request before the first remote mutation. Completed
    /// receipts use the same local replay/retirement policy as runtime actions.
    pub async fn begin(
        &self,
        operation: &JsString,
        request: &str,
        now: i64,
    ) -> IvyResult<Option<IvyJson>> {
        let operation = operation.clone();
        let request = request.to_owned();
        self.store
            .call(move |store| {
                let id = String::from_utf16_lossy(&operation.0);
                if store.operation(&id)?.is_some_and(|v| !v.is_null()) {
                    return store
                        .mutate(&id, &request, now, |_| Ok(IvyJson::Null))
                        .map(Some);
                }
                let request_key = key("definition-request:", &operation, None);
                let retained = store.metadata(&request_key)?.filter(|s| !s.is_empty());
                require(
                    retained.as_ref().is_none_or(|v| v == &request),
                    "operation_conflict",
                    "An operation identity cannot change its definition request.",
                )?;
                if retained.is_none() {
                    store.set_metadata(&request_key, &request)?;
                    store.set_metadata(
                        &key("definition-expiry:", &operation, None),
                        &(now + 7 * 86400000).to_string(),
                    )?;
                }
                Ok(None)
            })
            .await
    }
    pub async fn finish(
        &self,
        operation: &JsString,
        request: &str,
        result: IvyJson,
        now: i64,
    ) -> IvyResult<IvyJson> {
        let id = String::from_utf16_lossy(&operation.0);
        let request = request.to_owned();
        self.store
            .call(move |store| store.mutate(&id, &request, now, |_| Ok(result)))
            .await
    }
}
