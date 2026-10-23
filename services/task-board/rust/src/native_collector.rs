//! Incremental immutable native evidence with a CAS checkpoint. Collection never
//! publishes task workflow state or grants native dispatch authority.
use crate::{
    evidence_bytes::MAXIMUM_BYTES,
    full_turn_evidence::{FullTurnEvidence, first_items_params, full_turn_params},
    native_inspector::{Inspection, InspectionContext, InspectionStore, NativeInspector, PageSize},
    native_turn_evidence::{TurnEvidence, TurnEvidenceStore},
    operations::{Document, mutation},
    read_evidence::ReadIdentity,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts, hash::hash_json, json::canonical,
};
use std::collections::HashSet;

/// Names are stored metadata, including for a competing writer's retained
/// receipt. They cannot be reconstructed from the caller's proposed label.
#[allow(async_fn_in_trait)]
pub trait CollectionStore: InspectionStore + TurnEvidenceStore {
    fn document_name(&self, document: &Document) -> IvyResult<JsString>;
    async fn named_receipt(
        &self,
        key: &str,
        name: &JsString,
        parent: &JsString,
    ) -> IvyResult<Option<Document>>;
}
pub struct NativeCollector<'a, S> {
    pub store: &'a S,
    pub catalogs: &'a CheckedNativeContracts,
}
pub struct FullTurnCollector<'a, S> {
    pub store: &'a S,
    pub catalogs: &'a CheckedNativeContracts,
}
#[derive(Debug)]
pub enum NativeCollection {
    Pending { transcript: Json },
    Complete { artifact: Json },
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn number(v: &Json, k: &str) -> f64 {
    v.get(k).and_then(Json::as_f64).unwrap_or(f64::NAN)
}
fn unsupported(v: &Json) -> bool {
    field(field(v, "reply"), "error")
        .get("code")
        .and_then(Json::as_f64)
        == Some(-32601.0)
}
fn reference(observed: &Json, artifact: Json) -> IvyResult<Json> {
    Ok(Json::object([
        ("artifact", artifact),
        ("observationId", observed.required("observationId")?.clone()),
        ("epoch", observed.required("epoch")?.clone()),
    ]))
}
fn artifact<S: CollectionStore>(store: &S, document: &Document) -> IvyResult<Json> {
    Ok(Json::object([
        ("object", document.pin.clone()),
        ("label", Json::String(store.document_name(document)?)),
        ("contentHash", document.content_hash.clone().into()),
        ("mediaType", "application/json".into()),
    ]))
}
impl<S: CollectionStore> FullTurnCollector<'_, S> {
    fn name(collection: &Json) -> IvyResult<JsString> {
        let mut name = JsString::from("Native full turn transcript ");
        name.0.extend_from_slice(&s(collection, "collectionId")?.0);
        Ok(name)
    }
    pub async fn retained(&self, run: &Json, collection: &Json) -> IvyResult<Option<Json>> {
        let Some(document) = self
            .store
            .named_receipt(
                "task-board/native-full-turn-transcript",
                &Self::name(collection)?,
                s(run, "objectId")?,
            )
            .await?
        else {
            return Ok(None);
        };
        require(
            document.value.get("collectionId") == collection.get("collectionId")
                && field(&document.value, "snapshot").get("contentHash")
                    == field(collection, "snapshot").get("contentHash"),
            "task_board_evidence_mismatch",
            "A retained full transcript must belong to the original collection and snapshot.",
        )?;
        let result = artifact(self.store, &document)?;
        let principal = self.store.principal_id();
        FullTurnEvidence {
            turns: TurnEvidence {
                store: self.store,
                catalogs: self.catalogs,
                principal_id: &principal,
            },
        }
        .transcript(run, &result)
        .await?;
        Ok(Some(result))
    }
    async fn save(
        &self,
        run: &Json,
        observed: &Json,
        params: Json,
        context: &InspectionContext,
    ) -> IvyResult<Json> {
        let principal = self.store.principal_id();
        let artifact = TurnEvidence {
            store: self.store,
            catalogs: self.catalogs,
            principal_id: &principal,
        }
        .reads()
        .save(
            run,
            observed,
            &ReadIdentity {
                observation_id: s(observed, "observationId")?.clone(),
                caller_principal_id: principal.clone(),
                service_node_id: context.service_node_id.clone(),
                native_version: context.native_version.clone(),
                epoch: context.epoch.clone(),
                method: s(observed, "method")?.clone(),
                params,
            },
        )
        .await?;
        reference(observed, artifact)
    }
    pub async fn collect(
        &self,
        run: &Json,
        collection: &Document,
        original_error: &Json,
        budget: f64,
    ) -> IvyResult<Option<Json>> {
        let value = &collection.value;
        let principal = self.store.principal_id();
        let evidence = FullTurnEvidence {
            turns: TurnEvidence {
                store: self.store,
                catalogs: self.catalogs,
                principal_id: &principal,
            },
        };
        let snapshot = evidence
            .turns
            .snapshot(run, value.required("snapshot")?)
            .await?
            .document
            .value;
        require(
            value.get("phase") == Some(&Json::from("collecting"))
                && number(value, "pageCount") == 0.0
                && value.get("head") == Some(&Json::Null)
                && value.get("nextCursor") == Some(&Json::Null)
                && unsupported(original_error),
            "task_board_native_evidence_required",
            "Only an unsupported first item page permits full-turn collection.",
        )?;
        let inspector = NativeInspector {
            store: self.store,
            catalogs: self.catalogs,
        };
        let context = inspector.context(value.required("run")?).await?;
        require(
            Some(&context.epoch) == snapshot.get("epoch").and_then(Json::as_js_string),
            "task_board_native_epoch_changed",
            "Full-turn collection requires the original native epoch.",
        )?;
        // Retain this exact unsupported reply even if search exhausts this pass.
        let unsupported_items = self
            .save(
                value.required("run")?,
                original_error,
                first_items_params(s(&snapshot, "threadId")?, s(&snapshot, "turnId")?),
                &context,
            )
            .await?;
        let found = inspector
            .find(value.required("run")?, budget, PageSize::One)
            .await?;
        let turns = match found {
            Inspection::Pending { .. } => return Ok(None),
            Inspection::Found { turns, .. } => turns,
            Inspection::Absent => {
                return Err(IvyError::new(
                    "task_board_turn_unavailable",
                    "The original terminal turn is unavailable for full evidence.",
                ));
            }
        };
        let cursor = turns
            .required("params")?
            .get("cursor")
            .and_then(Json::as_js_string);
        let full_params = full_turn_params(s(&snapshot, "threadId")?, cursor, true);
        let verify_params = full_turn_params(s(&snapshot, "threadId")?, cursor, false);
        let full = inspector
            .read(
                &context,
                &"thread/turns/list".into(),
                full_params.clone(),
                false,
            )
            .await?;
        let full_turn = self
            .save(value.required("run")?, &full, full_params, &context)
            .await?;
        let checked = inspector
            .read(
                &context,
                &"thread/turns/list".into(),
                verify_params.clone(),
                false,
            )
            .await?;
        let verification = self
            .save(value.required("run")?, &checked, verify_params, &context)
            .await?;
        let complete = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("collectionId", value.required("collectionId")?.clone()),
            ("run", value.required("run")?.clone()),
            ("snapshot", value.required("snapshot")?.clone()),
            ("unsupportedItems", unsupported_items),
            ("fullTurn", full_turn),
            (
                "fullTurnCursor",
                cursor.cloned().map(Json::String).unwrap_or(Json::Null),
            ),
            (
                "fullTurnBytes",
                Json::Number(canonical(&full, MAXIMUM_BYTES)?.len() as f64),
            ),
            ("verification", verification),
            (
                "verificationCursor",
                cursor.cloned().map(Json::String).unwrap_or(Json::Null),
            ),
            ("createdAt", checked.required("observedAt")?.clone()),
        ]);
        evidence.verify(run, &complete).await?;
        self.store.verify_owner().await?;
        let result = self
            .store
            .write_document(
                "task-board/native-full-turn-transcript",
                &complete,
                &mutation(s(value, "collectionId")?, "full-turn:create")?,
                &Json::object([(
                    "create",
                    Json::object([
                        ("parentId", run.required("objectId")?.clone()),
                        ("name", Json::String(Self::name(value)?)),
                    ]),
                )]),
            )
            .await;
        if let Err(error) = result {
            if error.code != "mutation_conflict" {
                return Err(error);
            }
        }
        let result = self.retained(run, value).await?;
        require(
            result.is_some(),
            "task_board_evidence_mismatch",
            "A completed full transcript must remain available.",
        )?;
        Ok(result)
    }
}
impl<S: CollectionStore> NativeCollector<'_, S> {
    pub async fn begin(&self, run: &Json, snapshot_artifact: &Json) -> IvyResult<Json> {
        self.store.verify_owner().await?;
        let principal = self.store.principal_id();
        let snapshot = TurnEvidence {
            store: self.store,
            catalogs: self.catalogs,
            principal_id: &principal,
        }
        .snapshot(run, snapshot_artifact)
        .await?;
        let source = &snapshot.document.value;
        require(
            source.get("status") != Some(&Json::from("inProgress")),
            "task_board_native_order",
            "Item collection requires an observed terminal turn.",
        )?;
        let identity = Json::object([
            ("epoch", source.required("epoch")?.clone()),
            ("nativeVersion", source.required("nativeVersion")?.clone()),
            ("threadId", source.required("threadId")?.clone()),
            ("turnId", source.required("turnId")?.clone()),
        ]);
        let collection_id = mutation(
            s(run, "objectId")?,
            &format!("native-collection:{}", hash_json(&identity)?),
        )?;
        let name = format!("Native transcript {collection_id}");
        if let Some(existing) = self
            .store
            .named_document("task-board/native-transcript", &name, s(run, "objectId")?)
            .await?
        {
            require(
                existing.value.get("collectionId") == Some(&Json::from(collection_id)),
                "task_board_evidence_mismatch",
                "The retained transcript must preserve its collection identity.",
            )?;
            return Ok(existing.pin);
        }
        let value = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("collectionId", collection_id.clone().into()),
            ("run", source.required("run")?.clone()),
            ("snapshot", snapshot_artifact.clone()),
            ("phase", "collecting".into()),
            ("head", Json::Null),
            ("pageCount", Json::Number(0.0)),
            ("nativeBytes", Json::Number(0.0)),
            ("nextCursor", Json::Null),
            ("verification", Json::Null),
            ("verificationCursor", Json::Null),
            ("createdAt", source.required("createdAt")?.clone()),
            ("updatedAt", source.required("createdAt")?.clone()),
        ]);
        match self
            .store
            .write_document(
                "task-board/native-transcript",
                &value,
                &mutation(&collection_id.into(), "create")?,
                &Json::object([(
                    "create",
                    Json::object([
                        ("parentId", run.required("objectId")?.clone()),
                        ("name", name.clone().into()),
                    ]),
                )]),
            )
            .await
        {
            Ok(pin) => Ok(pin),
            Err(error) if error.code == "mutation_conflict" => {
                let winner = self
                    .store
                    .named_document("task-board/native-transcript", &name, s(run, "objectId")?)
                    .await?;
                require(
                    winner.as_ref().and_then(|d| d.value.get("collectionId"))
                        == value.get("collectionId"),
                    "task_board_evidence_mismatch",
                    "A competing collection must retain the same original identity.",
                )?;
                Ok(winner.ok_or_else(IvyError::internal)?.pin)
            }
            Err(error) => Err(error),
        }
    }
    async fn cursors(&self, document: &Document, snapshot: &Json) -> IvyResult<HashSet<String>> {
        let value = &document.value;
        let mut pages = Vec::new();
        let mut seen = HashSet::new();
        let mut pin = value.required("head")?.clone();
        while pin != Json::Null {
            require(
                (pages.len() as f64) < number(value, "pageCount")
                    && seen.insert(s(&pin, "objectId")?.clone()),
                "task_board_evidence_mismatch",
                "Incomplete page chain repeats or exceeds its declared bound.",
            )?;
            let page = self
                .store
                .read_document("task-board/native-result-page", &pin, false)
                .await?;
            pin = page.value.required("previous")?.clone();
            pages.push(page);
        }
        require(
            pages.len() as f64 == number(value, "pageCount"),
            "task_board_evidence_mismatch",
            "Every checkpoint page must remain available.",
        )?;
        pages.reverse();
        let mut cursors = HashSet::new();
        let mut cursor = Json::Null;
        let mut bytes = 0.0;
        for (index, page) in pages.iter().enumerate() {
            let v = &page.value;
            require(
                page.parent_id.as_ref() == Some(s(value.required("run")?, "objectId")?)
                    && v.get("runId") == field(value, "run").get("objectId")
                    && v.get("collectionId") == value.get("collectionId")
                    && number(v, "index") == index as f64 + 1.0
                    && v.get("cursor") == Some(&cursor)
                    && (index == 0 || cursor != Json::Null)
                    && field(v, "evidence").get("epoch") == snapshot.get("epoch")
                    && !cursors.contains(&hash_json(&cursor)?),
                "task_board_evidence_mismatch",
                "Checkpoint pages must preserve their exact scope, epoch and advancing cursor chain.",
            )?;
            cursors.insert(hash_json(&cursor)?);
            cursor = v.required("nextCursor")?.clone();
            bytes += number(v, "nativeBytes");
        }
        require(
            value.get("nextCursor") == Some(&cursor)
                && bytes == number(value, "nativeBytes")
                && (cursor == Json::Null || !cursors.contains(&hash_json(&cursor)?)),
            "task_board_evidence_mismatch",
            "Checkpoint cursor and bytes must match its retained page chain.",
        )?;
        Ok(cursors)
    }
    async fn checkpoint(&self, document: &Document, value: &Json) -> IvyResult<Document> {
        self.store.verify_owner().await?;
        let pin = self
            .store
            .write_document(
                "task-board/native-transcript",
                value,
                &mutation(
                    s(value, "collectionId")?,
                    &format!(
                        "checkpoint:{}",
                        hash_json(&Json::object([
                            ("pin", document.pin.clone()),
                            ("value", value.clone())
                        ]))?
                    ),
                )?,
                &Json::object([
                    ("objectId", document.pin.required("objectId")?.clone()),
                    (
                        "expectedRevision",
                        document.pin.required("revision")?.clone(),
                    ),
                ]),
            )
            .await?;
        self.store
            .read_document("task-board/native-transcript", &pin, false)
            .await
    }
    pub async fn advance(
        &self,
        run: &Json,
        object_id: &JsString,
        budget: f64,
    ) -> IvyResult<NativeCollection> {
        require(
            budget.is_finite() && budget.fract() == 0.0 && (1.0..=64.0).contains(&budget),
            "task_board_inspection_budget",
            "One collection pass allows one to 64 item pages.",
        )?;
        self.store.verify_owner().await?;
        let mut document = self
            .store
            .read_document(
                "task-board/native-transcript",
                &Json::String(object_id.clone()),
                false,
            )
            .await?;
        require(
            document.parent_id.as_ref() == Some(s(run, "objectId")?)
                && field(&document.value, "run").get("objectId") == run.get("objectId")
                && number(field(&document.value, "run"), "revision") <= number(run, "revision"),
            "task_board_evidence_mismatch",
            "Collection must retain its original Run scope.",
        )?;
        let principal = self.store.principal_id();
        let evidence = TurnEvidence {
            store: self.store,
            catalogs: self.catalogs,
            principal_id: &principal,
        };
        if document.value.get("phase") == Some(&Json::from("complete")) {
            let artifact = artifact(self.store, &document)?;
            evidence.transcript(run, &artifact).await?;
            return Ok(NativeCollection::Complete { artifact });
        }
        let snapshot = evidence
            .snapshot(run, document.value.required("snapshot")?)
            .await?
            .document
            .value;
        require(
            snapshot.get("status") != Some(&Json::from("inProgress"))
                && document.value.get("verification") == Some(&Json::Null)
                && document.value.get("verificationCursor") == Some(&Json::Null)
                && hash_json(snapshot.required("run")?)?
                    == hash_json(document.value.required("run")?)?,
            "task_board_evidence_mismatch",
            "An incomplete collection must retain its original terminal snapshot.",
        )?;
        let full = FullTurnCollector {
            store: self.store,
            catalogs: self.catalogs,
        };
        if number(&document.value, "pageCount") == 0.0 {
            if let Some(artifact) = full.retained(run, &document.value).await? {
                return Ok(NativeCollection::Complete { artifact });
            }
        }
        let inspector = NativeInspector {
            store: self.store,
            catalogs: self.catalogs,
        };
        let context = inspector.context(document.value.required("run")?).await?;
        require(
            Some(&context.epoch) == snapshot.get("epoch").and_then(Json::as_js_string),
            "task_board_native_epoch_changed",
            "A replacement owner cannot append to an earlier native collection.",
        )?;
        let mut cursors = self.cursors(&document, &snapshot).await?;
        for _ in 0..budget as usize {
            let current = &document.value;
            if number(current, "pageCount") != 0.0 && current.get("nextCursor") == Some(&Json::Null)
            {
                break;
            }
            require(
                number(current, "pageCount") < 1024.0,
                "task_board_native_collection_limit",
                "Native collection exceeded its 1024-page bound.",
            )?;
            require(
                !cursors.contains(&hash_json(current.required("nextCursor")?)?),
                "task_board_native_cursor_loop",
                "A native item cursor must advance.",
            )?;
            let params = Json::object([
                ("threadId", snapshot.required("threadId")?.clone()),
                ("turnId", snapshot.required("turnId")?.clone()),
                ("cursor", current.required("nextCursor")?.clone()),
                ("limit", Json::Number(100.0)),
                ("sortDirection", "asc".into()),
            ]);
            let observed = inspector
                .read(&context, &"thread/items/list".into(), params, true)
                .await?;
            if unsupported(&observed) && number(current, "pageCount") == 0.0 {
                return Ok(
                    match full.collect(run, &document, &observed, budget).await? {
                        Some(artifact) => NativeCollection::Complete { artifact },
                        None => NativeCollection::Pending {
                            transcript: document.pin,
                        },
                    },
                );
            }
            require(
                field(&observed, "reply").get("result").is_some(),
                "task_board_native_read_failed",
                "Native inspection returned its original error; no successful page is available.",
            )?;
            let head = evidence
                .save_page(
                    current.required("run")?,
                    &snapshot,
                    s(current, "collectionId")?,
                    current.required("head")?,
                    number(current, "pageCount") + 1.0,
                    current.get("nextCursor").and_then(Json::as_js_string),
                    &observed,
                )
                .await?;
            let page = self
                .store
                .read_document("task-board/native-result-page", &head, false)
                .await?;
            cursors.insert(hash_json(current.required("nextCursor")?)?);
            require(
                page.value.get("nextCursor") == Some(&Json::Null)
                    || !cursors.contains(&hash_json(page.value.required("nextCursor")?)?),
                "task_board_native_cursor_loop",
                "The native item reply repeated a collected cursor.",
            )?;
            require(
                number(current, "nativeBytes") + number(&page.value, "nativeBytes") <= 1073741824.0,
                "task_board_native_collection_limit",
                "Native collection exceeded its one-GiB page observation bound.",
            )?;
            let mut value = current.clone();
            value.insert("head", head);
            value.insert(
                "pageCount",
                Json::Number(number(current, "pageCount") + 1.0),
            );
            value.insert(
                "nativeBytes",
                Json::Number(number(current, "nativeBytes") + number(&page.value, "nativeBytes")),
            );
            value.insert("nextCursor", page.value.required("nextCursor")?.clone());
            value.insert("updatedAt", observed.required("observedAt")?.clone());
            document = self.checkpoint(&document, &value).await?;
        }
        if document.value.get("nextCursor") != Some(&Json::Null) {
            return Ok(NativeCollection::Pending {
                transcript: document.pin,
            });
        }
        let turns = match inspector
            .find(document.value.required("run")?, budget, PageSize::Hundred)
            .await?
        {
            Inspection::Pending { .. } => {
                return Ok(NativeCollection::Pending {
                    transcript: document.pin,
                });
            }
            Inspection::Found { turns, .. } => turns,
            Inspection::Absent => {
                return Err(IvyError::new(
                    "task_board_turn_unavailable",
                    "The original terminal turn is unavailable for final verification.",
                ));
            }
        };
        require(
            turns.get("epoch") == snapshot.get("epoch"),
            "task_board_native_epoch_changed",
            "Final verification must remain in the collection epoch.",
        )?;
        let read = evidence
            .reads()
            .save(
                document.value.required("run")?,
                &turns,
                &ReadIdentity {
                    observation_id: s(&turns, "observationId")?.clone(),
                    caller_principal_id: principal.clone(),
                    service_node_id: context.service_node_id,
                    native_version: context.native_version,
                    epoch: context.epoch,
                    method: s(&turns, "method")?.clone(),
                    params: turns.required("params")?.clone(),
                },
            )
            .await?;
        let mut value = document.value.clone();
        value.insert("phase", "complete".into());
        value.insert("verification", reference(&turns, read)?);
        value.insert(
            "verificationCursor",
            turns.required("params")?.required("cursor")?.clone(),
        );
        value.insert("updatedAt", turns.required("observedAt")?.clone());
        evidence.verify_transcript(run, &value).await?;
        document = self.checkpoint(&document, &value).await?;
        Ok(NativeCollection::Complete {
            artifact: artifact(self.store, &document)?,
        })
    }
}
