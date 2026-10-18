//! Hive I/O for the public ChatBridge records. Local journal cleanup and change
//! notification happen in the enclosing store after a confirmed, non-replayed write.
use super::{Destination, Document, Pin, Reference};
use crate::schema::{READABLE_VERSIONS, VERSION};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, hash::hash_json};
use ivy_sdk::{
    operation_ids::scoped_operation_id,
    rpc::{RequestOptions, RpcClient},
};
use std::sync::Arc;

pub struct HiveStore {
    pub client: Arc<dyn RpcClient>,
}

pub struct WriteReceipt {
    pub pin: Pin,
    /// A recovered create conflict returns the original pin without running
    /// terminal-input cleanup or incrementing changeVersion again.
    pub replayed: bool,
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(v) => *v,
        Json::Number(v) => *v != 0.0 && !v.is_nan(),
        Json::String(v) => !v.0.is_empty(),
        _ => true,
    }
}
fn pin_of(value: &Json) -> IvyResult<Pin> {
    Ok(Pin {
        object_id: value.required("object")?.required("id")?.as_str()?,
        revision: value
            .required("revision")?
            .required("revision")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as i64,
    })
}
impl HiveStore {
    pub fn new(client: Arc<dyn RpcClient>) -> Self {
        Self { client }
    }
    pub async fn read(
        &self,
        key: &str,
        reference: Reference<'_>,
        parent: Option<&str>,
    ) -> IvyResult<Document> {
        let result = self
            .client
            .request("objects.read", reference.json(), RequestOptions::default())
            .await?;
        let metadata = result.required("object")?;
        let revision = result.required("revision")?;
        let content = result.required("content")?;
        require(
            metadata.get("contractKey") == Some(&Json::from(key))
                && READABLE_VERSIONS
                    .iter()
                    .any(|v| revision.get("contractVersion") == Some(&Json::from(*v)))
                && revision.get("mediaType") == Some(&Json::from("application/json"))
                && content.get("encoding") == Some(&Json::from("json")),
            "chat_contract_mismatch",
            "The selected Object is not an exact supported ChatBridge record.",
        )?;
        require(
            metadata.get("parentId") == Some(&parent.map(Json::from).unwrap_or(Json::Null))
                && !metadata.get("effectivelyArchived").is_some_and(truthy),
            "chat_scope_mismatch",
            "The selected ChatBridge record is outside the active configured scope.",
        )?;
        require(
            match reference {
                Reference::Current(_) => true,
                Reference::Pinned(pin) => {
                    revision.get("revision").and_then(Json::as_f64) == Some(pin.revision as f64)
                }
            },
            "chat_evidence_mismatch",
            "The selected revision differs from its immutable pin.",
        )?;
        Ok(Document {
            key: key.into(),
            pin: pin_of(&result)?,
            value: content.required("value")?.clone(),
            metadata: metadata.clone(),
            revision: revision.clone(),
        })
    }
    pub async fn named(
        &self,
        key: &str,
        name: &str,
        parent: Option<&str>,
    ) -> IvyResult<Option<Document>> {
        let parent_filter = match parent {
            None => Json::object([("op", "isNull".into()), ("field", "object.parentId".into())]),
            Some(parent) => Json::object([
                ("op", "eq".into()),
                ("field", "object.parentId".into()),
                ("value", parent.into()),
            ]),
        };
        let page = self
            .client
            .request(
                "objects.query",
                Json::object([
                    ("contractKey", key.into()),
                    ("includeArchived", true.into()),
                    (
                        "where",
                        Json::object([
                            ("op", "and".into()),
                            (
                                "args",
                                Json::Array(vec![
                                    parent_filter,
                                    Json::object([
                                        ("op", "eq".into()),
                                        ("field", "object.name".into()),
                                        ("value", name.into()),
                                    ]),
                                ]),
                            ),
                        ]),
                    ),
                    ("limit", Json::Number(2.0)),
                ]),
                RequestOptions::default(),
            )
            .await?;
        let items = page
            .required("items")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        require(
            items.len() <= 1 && !page.get("nextCursor").is_some_and(truthy),
            "chat_identity_conflict",
            "A ChatBridge durable name must resolve to one record across all versions.",
        )?;
        match items.first() {
            None => Ok(None),
            Some(item) => Ok(Some(
                self.read(
                    key,
                    Reference::Current(&item.required("objectId")?.as_str()?),
                    parent,
                )
                .await?,
            )),
        }
    }
    pub async fn write(
        &self,
        key: &str,
        value: &Json,
        mutation_id: &str,
        destination: Destination<'_>,
    ) -> IvyResult<WriteReceipt> {
        let mut references = Json::Object(vec![]);
        match key {
            "chat-bridge/input" => {
                if let Some(result) = value.get("result").filter(|v| truthy(v)) {
                    references.insert("result", result.clone());
                }
                for (index, image) in value
                    .required("payload")?
                    .required("images")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?
                    .iter()
                    .enumerate()
                {
                    references.insert(format!("image-{index}"), image.required("object")?.clone());
                }
            }
            "chat-bridge/result" => {
                for (index, part) in value
                    .required("textParts")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?
                    .iter()
                    .enumerate()
                {
                    references.insert(format!("text-{index}"), part.required("object")?.clone());
                }
            }
            "chat-bridge/reply" => {
                let origin = value.required("origin")?;
                if origin.get("kind") == Some(&Json::from("native")) {
                    references.insert("result", origin.required("result")?.clone());
                }
                for (index, artifact) in value
                    .required("artifacts")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?
                    .iter()
                    .enumerate()
                {
                    references.insert(
                        format!("artifact-{index}"),
                        artifact.required("object")?.clone(),
                    );
                }
            }
            _ => {}
        }
        let mutation_id = scoped_operation_id(
            self.client.clone(),
            &Json::Array(vec!["chat".into(), mutation_id.into()]),
        )
        .await?;
        let mut params = Json::object([
            ("mutationId", Json::String(mutation_id)),
            ("contractVersion", VERSION.into()),
            (
                "content",
                Json::object([("encoding", "json".into()), ("value", value.clone())]),
            ),
            ("references", references),
        ]);
        match &destination {
            Destination::Create { parent_id, name } => {
                params.insert(
                    "create",
                    Json::object([
                        ("parentId", parent_id.map(Json::from).unwrap_or(Json::Null)),
                        ("name", Json::from(*name)),
                        ("ownerObjectId", Json::Null),
                        ("contractKey", key.into()),
                    ]),
                );
            }
            Destination::Update {
                object_id,
                expected_revision,
            } => {
                params.insert("objectId", Json::from(*object_id));
                params.insert("expectedRevision", Json::Number(*expected_revision as f64));
            }
        }
        match self
            .client
            .request("objects.write", params, RequestOptions::default())
            .await
        {
            Ok(result) => Ok(WriteReceipt {
                pin: pin_of(&result)?,
                replayed: false,
            }),
            Err(error) => {
                let Destination::Create { parent_id, name } = destination else {
                    return Err(error);
                };
                if error.code != "revision_conflict" {
                    return Err(error);
                }
                let existing = self.named(key, name, parent_id).await?;
                let matches = match &existing {
                    Some(doc) => {
                        doc.pin.revision == 1 && hash_json(&doc.value)? == hash_json(value)?
                    }
                    None => false,
                };
                require(
                    matches,
                    "revision_conflict",
                    "The durable ChatBridge identity already contains different content.",
                )?;
                Ok(WriteReceipt {
                    pin: existing.unwrap().pin,
                    replayed: true,
                })
            }
        }
    }
    /// Used after the enclosing store has ruled out a local technical record.
    pub async fn require_available(&self, pin: &Pin) -> IvyResult<()> {
        let value = self
            .client
            .request("objects.read", pin.json(), RequestOptions::default())
            .await?;
        require(
            !value
                .required("object")?
                .get("effectivelyArchived")
                .is_some_and(truthy)
                && value
                    .required("revision")?
                    .get("revision")
                    .and_then(Json::as_f64)
                    == Some(pin.revision as f64),
            "chat_notice_source_unavailable",
            "A notice needs an existing unarchived source revision.",
        )
    }
}
