//! Immutable terminal snapshots, output receipts and ordered transcript proofs.
use crate::{
    evidence_bytes::MAXIMUM_BYTES,
    journal_store::{Contracts, JournalStore},
    operations::{Document, mutation},
    read_evidence::{ReadEvidence, ReadEvidenceStore, ReadIdentity},
    turn_output::{
        bounded_output_summary, final_output_text, output_params, page_cursor, turns_params,
    },
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, agent_schema::validate_agent,
    checked_native_contract::CheckedNativeContracts, hash::hash_json, json::canonical,
};
use ivy_sdk::{clock::parse_date, native_observations::NativeObservations, rpc::RpcClient};
use std::collections::HashSet;

/// A snapshot artifact uses its stored document name, including on replay.
#[allow(async_fn_in_trait)]
pub trait TurnEvidenceStore: ReadEvidenceStore {
    async fn read_named_document(
        &self,
        key: &str,
        address: &Json,
        primary: bool,
    ) -> IvyResult<(Document, JsString)>;
}
impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString> TurnEvidenceStore
    for JournalStore<'_, R, P, C, I>
{
    async fn read_named_document(
        &self,
        key: &str,
        address: &Json,
        primary: bool,
    ) -> IvyResult<(Document, JsString)> {
        let document = self.read_document(key, address, primary).await?;
        let name = self.journal.name(s(&document.pin, "objectId")?)?;
        Ok((document, name))
    }
}
pub struct TurnEvidence<'a, S> {
    pub store: &'a S,
    pub catalogs: &'a CheckedNativeContracts,
    pub principal_id: &'a JsString,
}
pub struct Source {
    pub run: Document,
    pub native_version: JsString,
    pub thread_id: JsString,
    pub turn_id: JsString,
}
pub struct Snapshot {
    pub document: Document,
    pub source: Source,
    pub thread: Json,
    pub turn: Json,
}
pub struct Completion {
    pub snapshot: Snapshot,
    pub status: JsString,
    pub summary: JsString,
}
pub struct Transcript {
    pub document: Document,
    pub completion: Completion,
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn field<'a>(v: &'a Json, k: &str) -> &'a Json {
    v.get(k).unwrap_or(&Json::Null)
}
fn array(v: &Json) -> &[Json] {
    v.as_array().unwrap_or(&[])
}
fn string(v: &Json) -> JsString {
    v.as_js_string().cloned().unwrap_or_default()
}
fn number(v: &Json, k: &str) -> f64 {
    v.get(k).and_then(Json::as_f64).unwrap_or(f64::NAN)
}
fn truthy(v: &Json) -> bool {
    match v {
        Json::Null => false,
        Json::Bool(v) => *v,
        Json::String(v) => !v.0.is_empty(),
        Json::Number(v) => *v != 0.0 && !v.is_nan(),
        _ => true,
    }
}
fn time(v: &Json) -> f64 {
    v.get("observedAt")
        .and_then(Json::as_js_string)
        .map(parse_date)
        .unwrap_or(f64::NAN)
}
fn same(a: Option<&Json>, b: Option<&Json>) -> IvyResult<bool> {
    match (a, b) {
        (Some(a), Some(b)) => Ok(canonical(a, MAXIMUM_BYTES)? == canonical(b, MAXIMUM_BYTES)?),
        _ => Ok(a.is_none() && b.is_none()),
    }
}
fn terminal(v: &Json) -> bool {
    ["completed", "failed", "interrupted"]
        .iter()
        .any(|s| v == &Json::from(*s))
}
fn reference(observation: &Json, artifact: Json) -> IvyResult<Json> {
    Ok(Json::object([
        ("artifact", artifact),
        (
            "observationId",
            observation.required("observationId")?.clone(),
        ),
        ("epoch", observation.required("epoch")?.clone()),
    ]))
}
pub(crate) fn transcript_summary(mut text: JsString) -> JsString {
    if String::from_utf16_lossy(&text.0).len() > 262144 {
        text.0.truncate(65536);
        if text.0.last().is_some_and(|u| (0xd800..=0xdbff).contains(u)) {
            text.0.pop();
        }
        text.0.extend(
            "\n\nThe complete native output is retained in the transcript artifact.".encode_utf16(),
        );
    }
    text
}
impl<S: TurnEvidenceStore> TurnEvidence<'_, S> {
    pub fn reads(&self) -> ReadEvidence<'_, S> {
        ReadEvidence {
            store: self.store,
            catalogs: self.catalogs,
        }
    }
    async fn run(&self, pin: &Json) -> IvyResult<Source> {
        let run = self
            .store
            .read_document("task-board/run", pin, true)
            .await?;
        let primary = field(&run.value, "primaryResourceRef");
        require(
            primary.get("namespace") == Some(&Json::from("codex"))
                && primary.get("kind") == Some(&Json::from("thread"))
                && primary.get("serviceNodeId") == field(&run.value, "target").get("serviceNodeId")
                && truthy(field(&run.value, "turnId"))
                && truthy(field(&run.value, "nativeEpoch")),
            "task_board_turn_mismatch",
            "Turn evidence requires the saved original primary and turn identity.",
        )?;
        Ok(Source {
            native_version: s(run.value.required("plan")?, "nativeVersion")?.clone(),
            thread_id: s(primary, "nativeId")?.clone(),
            turn_id: s(&run.value, "turnId")?.clone(),
            run,
        })
    }
    fn expected(
        &self,
        run: &Json,
        version: &JsString,
        reference: &Json,
        method: &str,
        params: Json,
    ) -> IvyResult<ReadIdentity> {
        Ok(ReadIdentity {
            observation_id: s(reference, "observationId")?.clone(),
            caller_principal_id: self.principal_id.clone(),
            service_node_id: s(run.required("target")?, "serviceNodeId")?.clone(),
            native_version: version.clone(),
            epoch: s(reference, "epoch")?.clone(),
            method: method.into(),
            params,
        })
    }
    fn select_turn(&self, reply: &Json, turn: &JsString) -> IvyResult<Json> {
        require(
            reply.get("result").is_some(),
            "task_board_native_evidence_required",
            "Native turn inspection requires its successful original reply.",
        )?;
        let matches: Vec<_> = array(field(reply.required("result")?, "data"))
            .iter()
            .filter(|v| v.get("id").and_then(Json::as_js_string) == Some(turn))
            .collect();
        require(
            matches.len() == 1,
            "task_board_turn_unavailable",
            "The page must contain exactly the original known turn.",
        )?;
        Ok(matches[0].clone())
    }
    pub async fn save_snapshot(
        &self,
        run_pin: &Json,
        metadata: &Json,
        turns: &Json,
    ) -> IvyResult<Json> {
        validate_agent("ReadObservation", metadata)?;
        validate_agent("ReadObservation", turns)?;
        let source = self.run(run_pin).await?;
        let cursor = page_cursor(turns.required("params")?.get("cursor"))?;
        require(
            metadata.get("epoch") == turns.get("epoch"),
            "task_board_native_epoch_changed",
            "Metadata and selected turn must be observed in the same native epoch.",
        )?;
        let selected = self.select_turn(turns.required("reply")?, &source.turn_id)?;
        let status = string(field(&selected, "status"));
        let metadata_expected = self.expected(
            &source.run.value,
            &source.native_version,
            metadata,
            "thread/read",
            Json::object([
                ("threadId", Json::String(source.thread_id.clone())),
                ("includeTurns", false.into()),
            ]),
        )?;
        let turns_expected = self.expected(
            &source.run.value,
            &source.native_version,
            turns,
            "thread/turns/list",
            turns_params(&source.thread_id, cursor.as_ref()),
        )?;
        let meta_artifact = self
            .reads()
            .save(run_pin, metadata, &metadata_expected)
            .await?;
        let turn_artifact = self.reads().save(run_pin, turns, &turns_expected).await?;
        let value = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("run", run_pin.clone()),
            ("nativeVersion", Json::String(source.native_version)),
            (
                "serviceNodeId",
                source
                    .run
                    .value
                    .required("target")?
                    .required("serviceNodeId")?
                    .clone(),
            ),
            ("epoch", metadata.required("epoch")?.clone()),
            ("threadId", Json::String(source.thread_id)),
            ("turnId", Json::String(source.turn_id)),
            ("turnCursor", cursor.map(Json::String).unwrap_or(Json::Null)),
            ("metadata", reference(metadata, meta_artifact)?),
            ("turn", reference(turns, turn_artifact)?),
            ("status", Json::String(status)),
            ("createdAt", turns.required("observedAt")?.clone()),
        ]);
        self.store.validate("NativeTurnSnapshot", &value)?;
        let hash = hash_json(&value)?;
        let pin = self
            .store
            .write_document(
                "task-board/native-turn-snapshot",
                &value,
                &mutation(s(run_pin, "objectId")?, &format!("turn-snapshot:{hash}"))?,
                &Json::object([(
                    "create",
                    Json::object([
                        ("parentId", run_pin.required("objectId")?.clone()),
                        (
                            "name",
                            format!("Native turn snapshot {}", &hash[7..]).into(),
                        ),
                    ]),
                )]),
            )
            .await?;
        let (document, name) = self
            .store
            .read_named_document("task-board/native-turn-snapshot", &pin, false)
            .await?;
        let artifact = Json::object([
            ("object", document.pin),
            ("label", Json::String(name)),
            ("contentHash", document.content_hash.into()),
            ("mediaType", "application/json".into()),
        ]);
        self.snapshot(run_pin, &artifact).await?;
        Ok(artifact)
    }
    pub async fn snapshot(&self, run_pin: &Json, artifact: &Json) -> IvyResult<Snapshot> {
        self.store.validate("Artifact", artifact)?;
        let source = self.run(run_pin).await?;
        let document = self
            .store
            .read_document(
                "task-board/native-turn-snapshot",
                artifact.required("object")?,
                false,
            )
            .await?;
        let v = &document.value;
        require(
            document.parent_id.as_ref() == Some(s(run_pin, "objectId")?)
                && artifact.get("contentHash") == Some(&Json::from(document.content_hash.clone()))
                && artifact.get("mediaType") == Some(&Json::from("application/json"))
                && field(v, "run").get("objectId") == run_pin.get("objectId")
                && number(field(v, "run"), "revision") <= number(run_pin, "revision")
                && v.get("nativeVersion").and_then(Json::as_js_string)
                    == Some(&source.native_version)
                && v.get("serviceNodeId")
                    == source.run.value.required("target")?.get("serviceNodeId")
                && v.get("threadId").and_then(Json::as_js_string) == Some(&source.thread_id)
                && v.get("turnId").and_then(Json::as_js_string) == Some(&source.turn_id)
                && field(v, "metadata").get("epoch") == v.get("epoch")
                && field(v, "turn").get("epoch") == v.get("epoch"),
            "task_board_evidence_mismatch",
            "The snapshot must retain the exact selected Run, owner, version, epoch, thread and turn.",
        )?;
        let original = self.run(v.required("run")?).await?;
        require(
            same(
                original.run.value.get("originalRequest"),
                source.run.value.get("originalRequest"),
            )? && original.thread_id == source.thread_id
                && original.turn_id == source.turn_id,
            "task_board_evidence_mismatch",
            "Historical snapshot scope cannot substitute another original execution identity.",
        )?;
        let metadata = self
            .reads()
            .read(
                v.required("run")?,
                v.required("metadata")?.required("artifact")?,
                &self.expected(
                    &source.run.value,
                    &source.native_version,
                    v.required("metadata")?,
                    "thread/read",
                    Json::object([
                        ("threadId", Json::String(source.thread_id.clone())),
                        ("includeTurns", false.into()),
                    ]),
                )?,
            )
            .await?;
        let turns = self
            .reads()
            .read(
                v.required("run")?,
                v.required("turn")?.required("artifact")?,
                &self.expected(
                    &source.run.value,
                    &source.native_version,
                    v.required("turn")?,
                    "thread/turns/list",
                    turns_params(
                        &source.thread_id,
                        v.get("turnCursor").and_then(Json::as_js_string),
                    ),
                )?,
            )
            .await?;
        require(
            metadata.required("reply")?.get("result").is_some(),
            "task_board_native_evidence_required",
            "Snapshot metadata must retain a successful native reply.",
        )?;
        let thread = field(metadata.required("reply")?.required("result")?, "thread").clone();
        let turn = self.select_turn(turns.required("reply")?, &source.turn_id)?;
        require(
            thread.get("id").and_then(Json::as_js_string) == Some(&source.thread_id)
                && thread.get("ephemeral") == Some(&Json::Bool(false))
                && turn.get("status") == v.get("status")
                && v.get("createdAt") == turns.get("observedAt")
                && time(&metadata).is_finite()
                && time(&turns).is_finite()
                && time(&metadata) <= time(&turns),
            "task_board_evidence_mismatch",
            "Snapshot metadata and status must agree with the original native bodies.",
        )?;
        Ok(Snapshot {
            document,
            source,
            thread,
            turn,
        })
    }
    pub async fn save_output(
        &self,
        run_pin: &Json,
        snapshot: &Json,
        cursor: Option<&JsString>,
        observation: &Json,
    ) -> IvyResult<Json> {
        let source = self.run(run_pin).await?;
        let params = output_params(&source.thread_id, &source.turn_id, cursor);
        let artifact = self
            .reads()
            .save(
                run_pin,
                observation,
                &self.expected(
                    &source.run.value,
                    &source.native_version,
                    observation,
                    "thread/items/list",
                    params,
                )?,
            )
            .await?;
        let output = Json::object([
            (
                "cursor",
                cursor.cloned().map(Json::String).unwrap_or(Json::Null),
            ),
            ("evidence", reference(observation, artifact)?),
        ]);
        self.completion(run_pin, snapshot, Some(&output)).await?;
        Ok(output)
    }
    pub async fn completion(
        &self,
        run_pin: &Json,
        artifact: &Json,
        output: Option<&Json>,
    ) -> IvyResult<Completion> {
        let snapshot = self.snapshot(run_pin, artifact).await?;
        let v = &snapshot.document.value;
        let source = &snapshot.source;
        require(
            terminal(field(v, "status")),
            "task_board_native_state_changed",
            "Completion requires the exact terminal native turn.",
        )?;
        let mut text = JsString::default();
        if let Some(output) = output.filter(|v| truthy(v)) {
            require(
                field(output, "evidence").get("epoch") == v.get("epoch"),
                "task_board_native_epoch_changed",
                "Final output must come from the terminal snapshot epoch.",
            )?;
            let evidence = output.required("evidence")?;
            let observed = self
                .reads()
                .read(
                    run_pin,
                    evidence.required("artifact")?,
                    &self.expected(
                        &source.run.value,
                        &source.native_version,
                        evidence,
                        "thread/items/list",
                        output_params(
                            &source.thread_id,
                            &source.turn_id,
                            output.get("cursor").and_then(Json::as_js_string),
                        ),
                    )?,
                )
                .await?;
            require(
                observed.required("reply")?.get("result").is_some()
                    && time(&observed).is_finite()
                    && time(&observed) >= parse_date(s(v, "createdAt")?),
                "task_board_native_evidence_required",
                "Final output requires a successful read following its terminal snapshot.",
            )?;
            let items = self.items(&source.native_version, &observed, &source.turn_id)?;
            if items.len() == 1 {
                text = final_output_text(&items[0]);
            }
            require(
                items.len() == 1 && !text.0.is_empty(),
                "task_board_native_evidence_required",
                "Final output must be one assistant answer from the exact turn.",
            )?;
        }
        let status = s(v, "status")?.clone();
        if text.0.is_empty() {
            text = format!("Native turn {}.", status.to_utf8()?).into();
        }
        Ok(Completion {
            snapshot,
            status,
            summary: bounded_output_summary(&text),
        })
    }
    fn items(&self, version: &JsString, observed: &Json, turn: &JsString) -> IvyResult<Vec<Json>> {
        let source = self.catalogs.checked_native_contract(
            version,
            Some(&Json::object([(
                "catalogHash",
                observed.required("catalogHash")?.clone(),
            )])),
        )?;
        NativeObservations::new(&source)
            .native_turn_items(observed.required("reply")?.required("result")?, turn)
    }
    pub async fn save_page(
        &self,
        run_pin: &Json,
        snapshot: &Json,
        collection: &JsString,
        previous: &Json,
        index: f64,
        cursor: Option<&JsString>,
        observation: &Json,
    ) -> IvyResult<Json> {
        require(
            observation.get("epoch") == snapshot.get("epoch"),
            "task_board_native_epoch_changed",
            "All result pages must come from the collection epoch.",
        )?;
        let source = self.run(run_pin).await?;
        let params = page_params(snapshot, cursor)?;
        let artifact = self
            .reads()
            .save(
                run_pin,
                observation,
                &self.expected(
                    &source.run.value,
                    &source.native_version,
                    observation,
                    "thread/items/list",
                    params,
                )?,
            )
            .await?;
        require(
            observation.required("reply")?.get("result").is_some(),
            "task_board_native_evidence_required",
            "A result page requires a successful native reply.",
        )?;
        let body = observation.required("reply")?.required("result")?;
        let next = page_cursor(body.get("nextCursor"))?;
        let value = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("runId", run_pin.required("objectId")?.clone()),
            ("collectionId", Json::String(collection.clone())),
            ("previous", previous.clone()),
            ("index", Json::Number(index)),
            (
                "cursor",
                cursor.cloned().map(Json::String).unwrap_or(Json::Null),
            ),
            ("nextCursor", next.map(Json::String).unwrap_or(Json::Null)),
            ("evidence", reference(observation, artifact)?),
            (
                "itemCount",
                Json::Number(array(field(body, "data")).len() as f64),
            ),
            (
                "nativeBytes",
                Json::Number(canonical(observation, MAXIMUM_BYTES)?.len() as f64),
            ),
        ]);
        self.store
            .write_document(
                "task-board/native-result-page",
                &value,
                &mutation(
                    s(run_pin, "objectId")?,
                    &format!("result-page:{}", hash_json(&value)?),
                )?,
                &Json::object([(
                    "create",
                    Json::object([
                        ("parentId", run_pin.required("objectId")?.clone()),
                        (
                            "name",
                            format!("Native result page {}", &hash_json(&value)?[7..]).into(),
                        ),
                    ]),
                )]),
            )
            .await
    }
    async fn page(
        &self,
        run_pin: &Json,
        snapshot: &Json,
        document: &Document,
        collection: &JsString,
    ) -> IvyResult<(Vec<Json>, f64, JsString)> {
        let value = &document.value;
        require(
            document.parent_id.as_ref() == Some(s(run_pin, "objectId")?)
                && value.get("runId") == run_pin.get("objectId")
                && value.get("collectionId").and_then(Json::as_js_string) == Some(collection)
                && field(value, "evidence").get("epoch") == snapshot.get("epoch"),
            "task_board_evidence_mismatch",
            "Native result page must belong to its exact Run and collection.",
        )?;
        let run = self
            .store
            .read_document("task-board/run", run_pin, true)
            .await?;
        let evidence = value.required("evidence")?;
        let observed = self
            .reads()
            .read(
                run_pin,
                evidence.required("artifact")?,
                &self.expected(
                    &run.value,
                    s(snapshot, "nativeVersion")?,
                    evidence,
                    "thread/items/list",
                    page_params(snapshot, value.get("cursor").and_then(Json::as_js_string))?,
                )?,
            )
            .await?;
        require(
            observed.required("reply")?.get("result").is_some(),
            "task_board_native_evidence_required",
            "Native result page must have its successful original reply.",
        )?;
        let result = observed.required("reply")?.required("result")?;
        let next = page_cursor(result.get("nextCursor"))?
            .map(Json::String)
            .unwrap_or(Json::Null);
        require(
            number(value, "itemCount") == array(field(result, "data")).len() as f64
                && value.get("nextCursor") == Some(&next)
                && number(value, "nativeBytes")
                    == canonical(&observed, MAXIMUM_BYTES)?.len() as f64,
            "task_board_evidence_mismatch",
            "Page cursor, item count and source bytes must match their exact native observation.",
        )?;
        let items = self.items(
            s(snapshot, "nativeVersion")?,
            &observed,
            s(snapshot, "turnId")?,
        )?;
        require(
            time(&observed).is_finite(),
            "task_board_evidence_mismatch",
            "A result page needs a valid native observation time.",
        )?;
        Ok((
            items,
            time(&observed),
            s(&observed, "observationId")?.clone(),
        ))
    }
    pub async fn transcript(&self, run_pin: &Json, artifact: &Json) -> IvyResult<Transcript> {
        self.store.validate("Artifact", artifact)?;
        let document = self
            .store
            .read_document(
                "task-board/native-transcript",
                artifact.required("object")?,
                false,
            )
            .await?;
        let v = &document.value;
        require(
            document.parent_id.as_ref() == Some(s(run_pin, "objectId")?)
                && artifact.get("contentHash") == Some(&Json::from(document.content_hash.clone()))
                && artifact.get("mediaType") == Some(&Json::from("application/json"))
                && v.get("phase") == Some(&Json::from("complete"))
                && field(v, "run").get("objectId") == run_pin.get("objectId")
                && number(field(v, "run"), "revision") <= number(run_pin, "revision")
                && truthy(field(v, "head"))
                && truthy(field(v, "verification")),
            "task_board_evidence_mismatch",
            "Final native material requires the exact complete transcript artifact.",
        )?;
        let completion = self.verify_transcript(run_pin, v).await?;
        Ok(Transcript {
            document,
            completion,
        })
    }
    pub async fn verify_transcript(&self, run_pin: &Json, value: &Json) -> IvyResult<Completion> {
        self.store.validate("NativeTranscript", value)?;
        require(
            value.get("phase") == Some(&Json::from("complete"))
                && field(value, "run").get("objectId") == run_pin.get("objectId")
                && number(field(value, "run"), "revision") <= number(run_pin, "revision")
                && truthy(field(value, "head"))
                && truthy(field(value, "verification")),
            "task_board_evidence_mismatch",
            "A complete proposal needs its exact Run scope, page head and final verification.",
        )?;
        let snapshot = self.snapshot(run_pin, value.required("snapshot")?).await?;
        let initial = &snapshot.document.value;
        require(
            terminal(field(initial, "status"))
                && same(initial.get("run"), value.get("run"))?
                && field(value, "verification").get("epoch") == initial.get("epoch"),
            "task_board_evidence_mismatch",
            "A transcript must retain its original terminal snapshot and final observation epoch.",
        )?;
        let mut pages = Vec::new();
        let mut seen = HashSet::new();
        let mut current = value.required("head")?.clone();
        while truthy(&current) {
            require(
                (pages.len() as f64) < number(value, "pageCount")
                    && seen.insert(s(&current, "objectId")?.clone()),
                "task_board_evidence_mismatch",
                "Native page chain repeats or exceeds its declared bound.",
            )?;
            let page = self
                .store
                .read_document("task-board/native-result-page", &current, false)
                .await?;
            current = page.value.required("previous")?.clone();
            pages.push(page);
        }
        require(
            pages.len() as f64 == number(value, "pageCount"),
            "task_board_evidence_mismatch",
            "Native page chain must contain every declared page.",
        )?;
        require(
            pages
                .iter()
                .map(|p| number(&p.value, "nativeBytes"))
                .sum::<f64>()
                == number(value, "nativeBytes"),
            "task_board_evidence_mismatch",
            "Declared page bytes must fit the complete collection bound before decoding item bodies.",
        )?;
        pages.reverse();
        let mut cursor = Json::Null;
        let mut bytes = 0.0;
        let mut text = JsString::default();
        let mut last_text = JsString::default();
        let mut latest = parse_date(s(initial, "createdAt")?);
        let mut observations = HashSet::from([
            s(initial.required("metadata")?, "observationId")?.clone(),
            s(initial.required("turn")?, "observationId")?.clone(),
        ]);
        let mut cursors = HashSet::new();
        let mut ids = HashSet::new();
        for (index, page) in pages.iter().enumerate() {
            require(
                number(&page.value, "index") == index as f64 + 1.0
                    && same(page.value.get("cursor"), Some(&cursor))?
                    && (index == 0 || cursor != Json::Null)
                    && (cursor == Json::Null || cursors.insert(string(&cursor))),
                "task_board_evidence_mismatch",
                "Result page indexes and cursors must form an exact advancing chain from null.",
            )?;
            let (items, observed_at, observation_id) = self
                .page(
                    value.required("run")?,
                    initial,
                    page,
                    s(value, "collectionId")?,
                )
                .await?;
            bytes += number(&page.value, "nativeBytes");
            require(
                observed_at >= latest && observations.insert(observation_id),
                "task_board_evidence_mismatch",
                "Item pages must be successive distinct observations after the terminal snapshot.",
            )?;
            latest = observed_at;
            for item in items {
                let id = string(field(&item, "id"));
                require(
                    !id.0.is_empty() && ids.insert(hash_json(&Json::String(id))?),
                    "task_board_evidence_mismatch",
                    "Native result pages must not repeat an item identity.",
                )?;
                if item.get("type") == Some(&Json::from("agentMessage")) {
                    if let Some(part) = item.get("text").and_then(Json::as_js_string) {
                        last_text = JsString(part.0[..part.0.len().min(262145)].to_vec());
                        if item.get("phase") == Some(&Json::from("final_answer"))
                            && text.0.len() <= 262144
                        {
                            let length = (262145 - text.0.len()).min(last_text.0.len());
                            if !text.0.is_empty() {
                                text.0.extend([10, 10]);
                            }
                            text.0.extend_from_slice(&last_text.0[..length]);
                        }
                    }
                }
            }
            cursor = page.value.required("nextCursor")?.clone();
        }
        require(
            cursor == Json::Null
                && value.get("nextCursor") == Some(&Json::Null)
                && bytes == number(value, "nativeBytes"),
            "task_board_evidence_mismatch",
            "Final material needs native exhaustion and exact total source bytes.",
        )?;
        let verification = value.required("verification")?;
        let observed = self
            .reads()
            .read(
                value.required("run")?,
                verification.required("artifact")?,
                &self.expected(
                    &snapshot.source.run.value,
                    s(initial, "nativeVersion")?,
                    verification,
                    "thread/turns/list",
                    turns_params(
                        s(initial, "threadId")?,
                        value.get("verificationCursor").and_then(Json::as_js_string),
                    ),
                )?,
            )
            .await?;
        let verified = self.select_turn(observed.required("reply")?, s(initial, "turnId")?)?;
        require(
            verified.get("status") == initial.get("status")
                && time(&observed).is_finite()
                && time(&observed) >= latest
                && !observations.contains(s(&observed, "observationId")?),
            "task_board_native_state_changed",
            "A fresh distinct native completion observation must still match after item collection.",
        )?;
        let status = s(initial, "status")?.clone();
        if text.0.is_empty() {
            text = if !last_text.0.is_empty() {
                last_text
            } else {
                format!("Native turn {}.", status.to_utf8()?).into()
            };
        }
        Ok(Completion {
            snapshot,
            status,
            summary: transcript_summary(text),
        })
    }
}
fn page_params(snapshot: &Json, cursor: Option<&JsString>) -> IvyResult<Json> {
    Ok(Json::object([
        ("threadId", snapshot.required("threadId")?.clone()),
        ("turnId", snapshot.required("turnId")?.clone()),
        (
            "cursor",
            cursor.cloned().map(Json::String).unwrap_or(Json::Null),
        ),
        ("limit", Json::Number(100.0)),
        ("sortDirection", "asc".into()),
    ]))
}
