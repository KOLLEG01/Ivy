//! Immutable original-operation evidence. Saving a receipt does not dispatch a
//! native call, turn an error reply into a result, or infer Run completion.
use crate::{
    evidence_bytes::{self, CHUNK_BYTES, ChunkKey, MAXIMUM_BYTES},
    native_intent::{EvidenceIdentity, NativeIntent},
    native_plan::catalog,
    operations::{Store, mutation},
    read_evidence::{ReadEvidenceStore, manifest, parent_id},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts,
    hash::{digest, hash_json},
    json::canonical,
    native_contract::NativeContract,
};
use ivy_sdk::native_evidence::{
    NativeOperationIdentity, NativeOwnerIdentity, validate_native_operation,
};
use std::sync::Arc;

const KEY: &str = "task-board/native-evidence";
const HEADERS: [&str; 9] = [
    "operationId",
    "callerPrincipalId",
    "serviceNodeId",
    "nativeVersion",
    "nativeExecutableHash",
    "method",
    "requestHash",
    "phase",
    "epoch",
];

pub struct NativeEvidence<'a, S> {
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
fn text<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}

impl<S: Store + ReadEvidenceStore> NativeEvidence<'_, S> {
    fn intent(&self) -> NativeIntent<'_, S> {
        NativeIntent {
            store: self.store,
            catalogs: self.catalogs,
        }
    }

    fn verify(
        &self,
        value: &Json,
        expected: &EvidenceIdentity,
        source: &NativeContract,
    ) -> IvyResult<()> {
        if source.definition(&expected.method).is_err() {
            return Err(IvyError::new(
                "task_board_native_method_unsupported",
                "The evidence method is not in the exact checked native client catalog.",
            ));
        }
        validate_native_operation(
            value,
            &NativeOperationIdentity {
                owner: NativeOwnerIdentity {
                    service_node_id: expected.service_node_id.clone(),
                    host_id: None,
                    native_version: expected.native_version.clone(),
                    native_executable_hash: text(source.catalog(), "nativeExecutableHash")?.clone(),
                    catalog_hash: source.catalog_hash().into(),
                },
                caller_principal_id: expected.caller_principal_id.clone(),
                operation_id: expected.operation_id.clone(),
                method: expected.method.clone(),
                params: expected.params.clone(),
            },
            Some("task_board_evidence_mismatch"),
        )
    }

    async fn scope(
        &self,
        parent: &Json,
        expected: &EvidenceIdentity,
    ) -> IvyResult<Arc<NativeContract>> {
        // The original deliberately catches every NativeCall read failure and
        // then tries the same reference as a Run. Keep this exact boundary.
        let call = self
            .store
            .read_document("task-board/native-call", parent, true)
            .await
            .ok();
        if let Some(call) = call {
            let request = call.value.required("request")?;
            let source = catalog(self.catalogs, request)?;
            let mismatch =
                "NativeCall evidence must retain its saved exact request and checked definition.";
            require(
                call.value.get("operationId").and_then(Json::as_js_string)
                    == Some(&expected.operation_id)
                    && request.get("nativeVersion").and_then(Json::as_js_string)
                        == Some(&expected.native_version)
                    && request.get("catalogSourceHash") == source.catalog().get("sourceHash")
                    && request.get("method").and_then(Json::as_js_string) == Some(&expected.method),
                mismatch,
            )?;
            let resolved = self.intent().resolve_native_request(request)?;
            require(
                hash_json(resolved.required("params")?)? == hash_json(&expected.params)?,
                mismatch,
            )?;
            require(
                call.value
                    .get("expectedDefinitionHash")
                    .and_then(Json::as_js_string)
                    == Some(&hash_json(source.definition(&expected.method)?)?.into()),
                mismatch,
            )?;
            Ok(source)
        } else {
            let run = self
                .store
                .read_document("task-board/run", parent, true)
                .await?;
            require(
                run.value
                    .required("target")?
                    .get("serviceNodeId")
                    .and_then(Json::as_js_string)
                    == Some(&expected.service_node_id),
                "Run evidence must come from its original selected native owner.",
            )?;
            let plan = run.value.required("plan")?;
            require(
                plan.get("nativeVersion").and_then(Json::as_js_string)
                    == Some(&expected.native_version),
                "Run evidence must retain its planned native version.",
            )?;
            // The selection version comes from the expected identity. The prior
            // check makes it identical to the saved plan's native version.
            catalog(self.catalogs, plan)
        }
    }

    pub async fn save(
        &self,
        parent: &Json,
        observation: &Json,
        expected: &EvidenceIdentity,
    ) -> IvyResult<Json> {
        let source = self.scope(parent, expected).await?;
        self.verify(observation, expected, &source)?;
        // Operation evidence scopes and verifies before canonical byte admission;
        // native read evidence has a different, separately retained ordering.
        let body = canonical(observation, MAXIMUM_BYTES)?;
        let parent = parent_id(parent)?;
        let content_hash = digest(body.as_bytes());
        let chunks = evidence_bytes::save_chunks(
            self.store,
            parent,
            body.as_bytes(),
            ChunkKey::NativeEvidence,
        )?;
        let mut value = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("format", "agent-operation/canonical-json".into()),
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
        self.store.validate("NativeEvidence", &value)?;
        let name = format!("Native evidence {}", &content_hash[7..]);
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
                &mutation(parent, &format!("evidence:{content_hash}:manifest"))?,
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
        expected: &EvidenceIdentity,
    ) -> IvyResult<Json> {
        self.store.validate("Artifact", artifact)?;
        let source = self.scope(parent, expected).await?;
        let parent = parent_id(parent)?;
        let document = self
            .store
            .read_document(KEY, artifact.required("object")?, false)
            .await?;
        let value = &document.value;
        let matches = document.parent_id.as_ref() == Some(parent)
            && artifact.get("mediaType") == Some(&Json::from("application/json"))
            && artifact.get("contentHash") == Some(&Json::from(document.content_hash.clone()));
        let chunks = manifest(value)?;
        require(
            matches && chunks.chunks.len() == chunks.byte_length.div_ceil(CHUNK_BYTES),
            "Evidence must have the exact manifest scope, hash, media type and bounded ordered chunk count.",
        )?;
        let observation =
            evidence_bytes::read_evidence(self.store, parent, &chunks, ChunkKey::NativeEvidence)?;
        self.verify(&observation, expected, &source)?;
        for key in HEADERS {
            require(
                value.get(key) == observation.get(key),
                "The evidence header cannot contradict its validated native operation body.",
            )?;
        }
        Ok(observation)
    }
}
