//! Hive-backed dashboard persistence. Mutation receipts remain owned by Hive.
use crate::{sources, validation};
use futures_util::{StreamExt, stream::FuturesUnordered};
use ivy_contracts::{IvyError, IvyJson as J, IvyResult, JsString, json};
use ivy_sdk::rpc::{RequestOptions, RpcClient};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use subtle::ConstantTimeEq;

#[derive(Clone)]
pub struct DashboardStore {
    client: Arc<dyn RpcClient>,
    root: J,
    base: reqwest::Url,
    node: JsString,
}
impl DashboardStore {
    pub fn new(
        client: Arc<dyn RpcClient>,
        root: Option<JsString>,
        base: &str,
        node: JsString,
    ) -> IvyResult<Self> {
        Ok(Self {
            client,
            root: root.map(J::String).unwrap_or(J::Null),
            base: reqwest::Url::parse(base).map_err(|_| IvyError::internal())?,
            node,
        })
    }
    pub fn url(&self, id: &JsString) -> IvyResult<J> {
        let node = json::encode_uri_component(&self.node).map_err(|_| IvyError::internal())?;
        let id = json::encode_uri_component(id).map_err(|_| IvyError::internal())?;
        Ok(self
            .base
            .join(&format!("dashboards/#/view?node={node}&id={id}"))
            .map_err(|_| IvyError::internal())?
            .to_string()
            .into())
    }
    pub fn image_url(&self, id: &JsString, token: &J) -> IvyResult<J> {
        let Some(token) = token.as_js_string().filter(|s| !s.0.is_empty()) else {
            return Ok(J::Null);
        };
        let mut url = self
            .base
            .join("api/v1/dashboards/image.png")
            .map_err(|_| IvyError::internal())?;
        url.query_pairs_mut()
            .append_pair("node", &String::from_utf16_lossy(&self.node.0))
            .append_pair("id", &String::from_utf16_lossy(&id.0))
            .append_pair("token", &String::from_utf16_lossy(&token.0));
        Ok(url.to_string().into())
    }
    pub async fn read(&self, id: JsString) -> IvyResult<J> {
        let item = self
            .client
            .request(
                "objects.read",
                J::object([("objectId", J::String(id.clone()))]),
                RequestOptions::default(),
            )
            .await?;
        let object = item.required("object")?;
        let revision = item.required("revision")?;
        let content = item.required("content")?;
        let version = revision.required("contractVersion")?.as_str()?;
        if object.get("contractKey") != Some(&"dashboards/dashboard".into())
            || object.get("parentId") != Some(&self.root)
            || object.get("effectivelyArchived").and_then(J::as_bool) == Some(true)
            || !["1.0.0", "1.1.0"].contains(&version.as_str())
            || content.get("encoding") != Some(&"json".into())
        {
            return Err(IvyError::new(
                "target_conflict",
                "Object is not an active dashboard in this workspace.",
            ));
        }
        // Invalid stored content is a provider failure, not a caller input error.
        let value = validation::Definition::parse(content.required("value")?)
            .map_err(|_| IvyError::internal())?
            .into_json();
        let url = self.url(&id)?;
        let image_url = self.image_url(&id, value.required("imageToken")?)?;
        Ok(J::object([
            ("id", J::String(id)),
            ("revision", revision.required("revision")?.clone()),
            ("value", value),
            ("url", url),
            ("imageUrl", image_url),
        ]))
    }
    pub async fn image(&self, id: JsString, token: &JsString) -> IvyResult<J> {
        let row = self.read(id).await?;
        let stored = row
            .required("value")?
            .required("imageToken")?
            .as_js_string();
        let valid = stored.is_some_and(|stored| {
            let expected = Sha256::digest(String::from_utf16_lossy(&stored.0).as_bytes());
            let actual = Sha256::digest(String::from_utf16_lossy(&token.0).as_bytes());
            bool::from(expected.as_slice().ct_eq(actual.as_slice()))
        });
        if !valid {
            return Err(IvyError::new(
                "unauthenticated",
                "Invalid dashboard image token.",
            ));
        }
        Ok(row)
    }
    pub async fn list(&self, cursor: Option<JsString>, limit: u32) -> IvyResult<J> {
        let scope = if self.root.as_js_string().is_some_and(|s| !s.0.is_empty()) {
            J::object([
                ("op", "eq".into()),
                ("field", "object.parentId".into()),
                ("value", self.root.clone()),
            ])
        } else {
            J::object([("op", "isNull".into()), ("field", "object.parentId".into())])
        };
        let mut params = J::object([
            ("contractKey", "dashboards/dashboard".into()),
            (
                "contractVersions",
                J::Array(vec!["1.0.0".into(), "1.1.0".into()]),
            ),
            ("where", scope),
            ("limit", J::Number(f64::from(limit))),
        ]);
        if let Some(cursor) = cursor.filter(|s| !s.0.is_empty()) {
            params.insert("cursor", J::String(cursor));
        }
        let page = self
            .client
            .request("objects.query", params, RequestOptions::default())
            .await?;
        let mut rows = Vec::new();
        for chunk in page
            .required("items")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .chunks(16)
        {
            let mut pending = FuturesUnordered::new();
            for (index, item) in chunk.iter().enumerate() {
                let store = self.clone();
                let id = item
                    .required("objectId")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?
                    .clone();
                pending.push(tokio::spawn(async move { (index, store.read(id).await) }));
            }
            let mut batch = vec![J::Null; chunk.len()];
            while let Some(row) = pending.next().await {
                let (index, row) = row.map_err(|_| IvyError::internal())?;
                let row = row?;
                batch[index] = J::object([
                    ("id", row.required("id")?.clone()),
                    ("revision", row.required("revision")?.clone()),
                    ("title", row.required("value")?.required("title")?.clone()),
                    ("url", row.required("url")?.clone()),
                    ("imageUrl", row.required("imageUrl")?.clone()),
                ]);
            }
            rows.extend(batch);
        }
        Ok(J::object([
            ("items", J::Array(rows)),
            ("nextCursor", page.required("nextCursor")?.clone()),
        ]))
    }
    /// Value and selector pairs have already passed the public input parser.
    pub async fn save(
        &self,
        value: J,
        id: Option<JsString>,
        expected_revision: Option<f64>,
        operation: JsString,
    ) -> IvyResult<J> {
        if let Some(id) = &id {
            self.read(id.clone()).await?;
        }
        let mut params = J::object([
            ("mutationId", J::String(operation.clone())),
            ("contractVersion", "1.1.0".into()),
            ("references", J::Object(vec![])),
            (
                "content",
                J::object([("encoding", "json".into()), ("value", value.clone())]),
            ),
        ]);
        if let Some(id) = id {
            params.insert("objectId", J::String(id));
            params.insert(
                "expectedRevision",
                J::Number(expected_revision.ok_or_else(IvyError::internal)?),
            );
        } else {
            let mut name: JsString = "dashboard-".into();
            name.0.extend(operation.0.iter().map(|c| {
                if matches!(*c,65..=90|97..=122|48..=57|45) {
                    *c
                } else {
                    45
                }
            }));
            params.insert(
                "create",
                J::object([
                    ("name", J::String(name)),
                    ("contractKey", "dashboards/dashboard".into()),
                    ("parentId", self.root.clone()),
                    ("ownerObjectId", J::Null),
                ]),
            );
        }
        let result = self
            .client
            .request("objects.write", params, RequestOptions::default())
            .await?;
        let id = result
            .required("object")?
            .required("id")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        Ok(J::object([
            ("id", J::String(id.clone())),
            (
                "revision",
                result.required("revision")?.required("revision")?.clone(),
            ),
            ("url", self.url(id)?),
            (
                "imageUrl",
                self.image_url(id, value.required("imageToken")?)?,
            ),
        ]))
    }
    pub async fn remove(
        &self,
        id: JsString,
        expected_revision: f64,
        operation: JsString,
    ) -> IvyResult<J> {
        if let Err(error) = self.read(id.clone()).await {
            if error.code_js() != JsString::from("not_found") {
                return Err(error);
            }
        }
        self.client
            .request(
                "objects.delete",
                J::object([
                    ("objectId", J::String(id.clone())),
                    ("expectedRevision", J::Number(expected_revision)),
                    ("mutationId", J::String(operation)),
                ]),
                RequestOptions::default(),
            )
            .await?;
        Ok(J::object([
            ("id", J::String(id)),
            ("deleted", J::Bool(true)),
        ]))
    }
    pub async fn data(&self, value: &J) -> IvyResult<J> {
        let selectors = value
            .required("sources")?
            .as_object()
            .ok_or_else(IvyError::internal)?
            .iter()
            .map(|(name, source)| {
                let selector = if let Some(id) = source.get("objectId") {
                    sources::Source::Object(
                        id.as_js_string().ok_or_else(IvyError::internal)?.clone(),
                    )
                } else {
                    sources::Source::Contract {
                        key: source
                            .required("contractKey")?
                            .as_js_string()
                            .ok_or_else(IvyError::internal)?
                            .clone(),
                        limit: source
                            .required("limit")?
                            .as_f64()
                            .ok_or_else(IvyError::internal)? as u32,
                    }
                };
                Ok((name.clone(), selector))
            })
            .collect::<IvyResult<Vec<_>>>()?;
        sources::snapshot(self.client.clone(), &selectors, RequestOptions::default()).await
    }
}
