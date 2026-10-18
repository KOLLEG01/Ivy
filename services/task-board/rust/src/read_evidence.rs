//! Immutable native reads, scoped to their original Run and retained catalog.
use crate::{
    evidence_bytes::{self, BlobStore, Chunk, ChunkKey, MAXIMUM_BYTES, Manifest},
    operations::{Document, mutation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts,
    hash::{digest, hash_json},
    json::canonical,
};
use ivy_sdk::native_evidence::{NativeOwnerIdentity, NativeReadIdentity, validate_native_read};

const KEY: &str = "task-board/native-read-evidence";
const HEADERS: [&str; 9] = [
    "observationId",
    "callerPrincipalId",
    "serviceNodeId",
    "nativeVersion",
    "nativeExecutableHash",
    "catalogHash",
    "method",
    "requestHash",
    "epoch",
];

pub struct ReadIdentity {
    pub observation_id: JsString,
    pub caller_principal_id: JsString,
    pub service_node_id: JsString,
    pub native_version: JsString,
    pub epoch: JsString,
    pub method: JsString,
    pub params: Json,
}

/// The journal supplies its canonical TaskBoard validator and exact document
/// reads/writes. Blob operations run on that same journal's blocking context.
#[allow(async_fn_in_trait)]
pub trait ReadEvidenceStore: BlobStore<Pin = Json, Error = IvyError> {
    fn validate(&self, definition: &str, value: &Json) -> IvyResult<()>;
    async fn read_document(&self, key: &str, address: &Json, upgrade: bool) -> IvyResult<Document>;
    async fn write_document(
        &self,
        key: &str,
        value: &Json,
        mutation: &str,
        destination: &Json,
    ) -> IvyResult<Json>;
}

pub struct ReadEvidence<'a, S> {
    pub store: &'a S,
    pub catalogs: &'a CheckedNativeContracts,
}
fn require(ok: bool, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new("task_board_evidence_mismatch", message))
    }
}
fn string<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn parent_id(parent: &Json) -> IvyResult<&JsString> {
    if let Some(id) = parent.as_js_string() {
        Ok(id)
    } else {
        string(parent, "objectId")
    }
}
fn manifest(value: &Json) -> IvyResult<Manifest<Json>> {
    // Document reads already enforce their exact stored schema, including integer bounds.
    let number = |v: &Json, key: &str| -> IvyResult<usize> {
        v.required(key)?
            .as_f64()
            .map(|v| v as usize)
            .ok_or_else(IvyError::internal)
    };
    Ok(Manifest {
        byte_length: number(value, "byteLength")?,
        content_hash: value.required("contentHash")?.as_str()?,
        chunks: value
            .required("chunks")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .map(|chunk| {
                Ok(Chunk {
                    object: chunk.required("object")?.clone(),
                    content_hash: chunk.required("contentHash")?.as_str()?,
                    byte_length: number(chunk, "byteLength")?,
                })
            })
            .collect::<IvyResult<_>>()?,
    })
}

impl<S: ReadEvidenceStore> ReadEvidence<'_, S> {
    pub fn verify(
        &self,
        value: &Json,
        expected: &ReadIdentity,
        source_hash: Option<&JsString>,
    ) -> IvyResult<()> {
        // JS selects a catalog by source hash only when the optional string is truthy.
        let identity = source_hash
            .filter(|s| !s.0.is_empty())
            .map(|hash| Json::object([("sourceHash", Json::String(hash.clone()))]));
        let source = self
            .catalogs
            .checked_native_contract(&expected.native_version, identity.as_ref())?;
        validate_native_read(
            value,
            &NativeReadIdentity {
                owner: NativeOwnerIdentity {
                    service_node_id: expected.service_node_id.clone(),
                    host_id: None,
                    native_version: expected.native_version.clone(),
                    native_executable_hash: string(source.catalog(), "nativeExecutableHash")?
                        .clone(),
                    catalog_hash: source.catalog_hash().into(),
                },
                caller_principal_id: expected.caller_principal_id.clone(),
                epoch: expected.epoch.clone(),
                method: expected.method.clone(),
                params: expected.params.clone(),
            },
            Some("task_board_evidence_mismatch"),
        )?;
        require(
            value.get("observationId").and_then(Json::as_js_string)
                == Some(&expected.observation_id),
            "The read must retain its exact original observation ID.",
        )
    }
    async fn scope(&self, parent: &Json, expected: &ReadIdentity) -> IvyResult<JsString> {
        let run = self
            .store
            .read_document("task-board/run", parent, true)
            .await?;
        let plan = run.value.required("plan")?;
        require(
            string(run.value.required("target")?, "serviceNodeId")? == &expected.service_node_id
                && string(plan, "nativeVersion")? == &expected.native_version,
            "Read evidence must belong to its primary Run and original selected native owner/version.",
        )?;
        Ok(string(plan, "catalogSourceHash")?.clone())
    }
    pub async fn save(
        &self,
        parent: &Json,
        observation: &Json,
        expected: &ReadIdentity,
    ) -> IvyResult<Json> {
        // Preserve the original failure order: canonical admission precedes scope I/O.
        let body = canonical(observation, MAXIMUM_BYTES)?;
        let source_hash = self.scope(parent, expected).await?;
        self.verify(observation, expected, Some(&source_hash))?;
        let parent = parent_id(parent)?;
        let content_hash = digest(body.as_bytes());
        let chunks = evidence_bytes::save_chunks(
            self.store,
            parent,
            body.as_bytes(),
            ChunkKey::NativeReadEvidence,
        )?;
        let mut value = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("format", "agent-read-observation/canonical-json".into()),
        ]);
        for key in HEADERS {
            value.insert(key, observation.required(key)?.clone());
        }
        value.insert("contentHash", content_hash.clone().into());
        value.insert("byteLength", Json::Number(body.len() as f64));
        value.insert(
            "chunks",
            Json::Array(
                chunks
                    .into_iter()
                    .map(|chunk| {
                        Json::object([
                            ("object", chunk.object),
                            ("contentHash", chunk.content_hash.into()),
                            ("byteLength", Json::Number(chunk.byte_length as f64)),
                        ])
                    })
                    .collect(),
            ),
        );
        self.store.validate("NativeReadEvidence", &value)?;
        let name = format!("Native read evidence {}", &content_hash[7..]);
        let destination = Json::object([(
            "create",
            Json::object([
                ("parentId", Json::String(parent.clone())),
                ("name", name.clone().into()),
            ]),
        )]);
        let pin = self
            .store
            .write_document(
                KEY,
                &value,
                &mutation(parent, &format!("read-evidence:{content_hash}:manifest"))?,
                &destination,
            )
            .await?;
        Ok(Json::object([
            ("object", pin),
            ("label", name.into()),
            ("contentHash", hash_json(&value)?.into()),
            ("mediaType", "application/json".into()),
        ]))
    }
    pub async fn read(
        &self,
        parent: &Json,
        artifact: &Json,
        expected: &ReadIdentity,
    ) -> IvyResult<Json> {
        self.store.validate("Artifact", artifact)?;
        let source_hash = self.scope(parent, expected).await?;
        let parent = parent_id(parent)?;
        let document = self
            .store
            .read_document(KEY, artifact.required("object")?, false)
            .await?;
        require(
            document.parent_id.as_ref() == Some(parent)
                && artifact.get("mediaType").and_then(Json::as_js_string)
                    == Some(&"application/json".into())
                && artifact.get("contentHash").and_then(Json::as_js_string)
                    == Some(&document.content_hash.clone().into()),
            "Read evidence must have its exact manifest scope, hash and media type.",
        )?;
        let observation = evidence_bytes::read_evidence(
            self.store,
            parent,
            &manifest(&document.value)?,
            ChunkKey::NativeReadEvidence,
        )?;
        self.verify(&observation, expected, Some(&source_hash))?;
        for key in HEADERS {
            require(
                document.value.get(key) == observation.get(key),
                "Read evidence header contradicts its exact validated native body.",
            )?;
        }
        Ok(observation)
    }
}
