//! Original read-ledger reducer. Native schemas and ownership are verified by
//! the caller before append; timestamps never order native authority.
use crate::limits::{COLLECTION_BYTES, COLLECTION_READS, MAXIMUM_EVIDENCE_BYTES};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts,
    json::{canonical, try_js_string},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::native_observations::NativeObservations;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Search,
    Items,
    Full,
    Verify,
    Done,
}
impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Items => "items",
            Self::Full => "full",
            Self::Verify => "verify",
            Self::Done => "done",
        }
    }
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field<'a>(value: &'a Json, key: &str) -> IvyResult<&'a Json> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn receiver(value: Option<&Json>) -> IvyResult<&Json> {
    value
        .filter(|v| !v.is_null())
        .ok_or_else(IvyError::internal)
}
fn cursor_of(value: Option<&Json>) -> IvyResult<Option<JsString>> {
    match value {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(cursor)) if !cursor.0.is_empty() => Ok(Some(cursor.clone())),
        _ => Err(IvyError::new(
            "chat_collection_gap",
            "A native page needs an omitted/null final cursor or nonempty next cursor.",
        )),
    }
}
fn cursor_json(value: &Option<JsString>) -> Json {
    value.clone().map(Json::String).unwrap_or(Json::Null)
}

#[derive(Clone)]
pub struct ChatCollectionReads {
    pub thread_id: JsString,
    pub turn_id: JsString,
    pub phase: Phase,
    pub cursor: Option<JsString>,
    pub found_cursor: Option<JsString>,
    pub native_state: Json,
    pub native_bytes: usize,
    pub count: usize,
    pub text_bytes: usize,
    pub last_observed_at: Option<Json>,
    pub texts: Vec<JsString>,
    observations: HashSet<JsString>,
    cursors: HashSet<JsString>,
    turns: HashSet<JsString>,
    items: HashSet<JsString>,
}
impl ChatCollectionReads {
    pub fn new(thread_id: JsString, turn_id: JsString) -> Self {
        Self {
            thread_id,
            turn_id,
            phase: Phase::Search,
            cursor: None,
            found_cursor: None,
            native_state: Json::Null,
            native_bytes: 0,
            count: 0,
            text_bytes: 0,
            last_observed_at: Some(Json::Null),
            texts: Vec::new(),
            observations: HashSet::new(),
            cursors: HashSet::new(),
            turns: HashSet::new(),
            items: HashSet::new(),
        }
    }
    pub fn fork(&self) -> Self {
        self.clone()
    }
    pub fn next(&self) -> IvyResult<Json> {
        require(
            self.phase != Phase::Done,
            "chat_collection_complete",
            "Complete native evidence cannot append another read.",
        )?;
        if self.phase == Phase::Items {
            return Ok(Json::object([
                ("method", "thread/items/list".into()),
                (
                    "params",
                    Json::object([
                        ("threadId", Json::String(self.thread_id.clone())),
                        ("turnId", Json::String(self.turn_id.clone())),
                        ("cursor", cursor_json(&self.cursor)),
                        ("limit", Json::Number(8.0)),
                        ("sortDirection", "asc".into()),
                    ]),
                ),
            ]));
        }
        Ok(Json::object([
            ("method", "thread/turns/list".into()),
            (
                "params",
                Json::object([
                    ("threadId", Json::String(self.thread_id.clone())),
                    (
                        "cursor",
                        cursor_json(if self.phase == Phase::Full {
                            &self.found_cursor
                        } else {
                            &self.cursor
                        }),
                    ),
                    ("limit", Json::Number(1.0)),
                    (
                        "itemsView",
                        (if self.phase == Phase::Full {
                            "full"
                        } else {
                            "notLoaded"
                        })
                        .into(),
                    ),
                    ("sortDirection", "desc".into()),
                ]),
            ),
        ]))
    }
    fn advance(&mut self, next: Option<JsString>) -> IvyResult<()> {
        require(
            next.as_ref().is_none_or(|value| {
                self.cursor.as_ref() != Some(value) && !self.cursors.contains(value)
            }),
            "chat_collection_gap",
            "Native page cursors must advance without repeating.",
        )?;
        if let Some(cursor) = self.cursor.take() {
            self.cursors.insert(cursor);
        }
        self.cursor = next;
        Ok(())
    }
    fn verify_next(&mut self) {
        self.phase = Phase::Verify;
        self.cursor = None;
        self.cursors.clear();
        self.turns.clear();
    }
    fn append_items(&mut self, items: &[Json]) -> IvyResult<()> {
        for item in items {
            let item = receiver(Some(item))?;
            let id = item.get("id").and_then(Json::as_js_string);
            require(
                id.is_some_and(|id| !id.0.is_empty() && !self.items.contains(id)),
                "chat_collection_gap",
                "Complete native items must have distinct original identities.",
            )?;
            self.items.insert(id.unwrap().clone());
            if item.get("type") == Some(&"agentMessage".into())
                && item.get("phase") != Some(&"commentary".into())
            {
                let text = item.get("text").and_then(Json::as_js_string);
                let encoded = text.and_then(|text| text.to_utf8().ok());
                require(
                    encoded.is_some(),
                    "chat_result_text_invalid",
                    "Original reply text must encode losslessly as UTF-8.",
                )?;
                self.text_bytes +=
                    encoded.unwrap().len() + if self.texts.is_empty() { 0 } else { 2 };
                require(
                    self.text_bytes <= COLLECTION_BYTES,
                    "chat_collection_limit",
                    "Complete native reply text exceeds 64 MiB.",
                )?;
                self.texts.push(text.unwrap().clone());
            }
        }
        Ok(())
    }
    /// False is an in-progress observation, without extending the saved ledger.
    /// A failed append may have changed transient state, as in the original;
    /// the collector uses fork before deciding which observation to retain.
    pub fn append(
        &mut self,
        observed: &Json,
        catalogs: &CheckedNativeContracts,
    ) -> IvyResult<bool> {
        let next = self.next()?;
        let method_matches = observed.get("method") == next.get("method");
        require(
            method_matches
                && canonical(field(observed, "params")?, MANAGEMENT_FRAME_BYTES)?
                    == canonical(field(&next, "params")?, MANAGEMENT_FRAME_BYTES)?,
            "chat_collection_gap",
            "Retained observations must answer the exact next original query.",
        )?;
        let observation_id = field(observed, "observationId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        require(
            !self.observations.contains(observation_id),
            "chat_collection_gap",
            "Native completion requires distinct original read observations.",
        )?;
        let bytes = canonical(observed, MAXIMUM_EVIDENCE_BYTES)?.len();
        require(
            self.count < COLLECTION_READS && self.native_bytes + bytes <= COLLECTION_BYTES,
            "chat_collection_limit",
            "Complete native collection exceeds its read or byte bound.",
        )?;
        let reply = receiver(observed.get("reply"))?;
        if let Some(error) = reply.get("error") {
            let error = receiver(Some(error))?;
            require(
                self.phase == Phase::Items
                    && self.cursor.is_none()
                    && self.items.is_empty()
                    && error.get("code") == Some(&Json::Number(-32601.0)),
                "chat_collection_native_error",
                "The original native result read failed; only first-page unsupported items permit full-turn collection.",
            )?;
            self.phase = Phase::Full;
        } else {
            let body = receiver(reply.get("result"))?;
            let next_cursor = cursor_of(body.get("nextCursor"))?;
            let data = body.get("data").and_then(Json::as_array);
            require(
                data.is_some(),
                "chat_collection_gap",
                "A native page must retain its original data array.",
            )?;
            let data = data.unwrap();
            if self.phase == Phase::Items {
                require(
                    !data.is_empty() || next_cursor.is_none(),
                    "chat_collection_gap",
                    "An empty nonfinal item page does not prove a complete native result.",
                )?;
                let version = field(observed, "nativeVersion")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?;
                let identity =
                    Json::object([("catalogHash", field(observed, "catalogHash")?.clone())]);
                let catalog = catalogs.checked_native_contract(version, Some(&identity))?;
                let items =
                    NativeObservations::new(&catalog).native_turn_items(body, &self.turn_id)?;
                self.append_items(&items)?;
                self.advance(next_cursor.clone())?;
                if next_cursor.is_none() {
                    self.verify_next();
                }
            } else {
                require(
                    data.len() <= 1,
                    "chat_collection_gap",
                    "A one-turn query must return at most its requested one turn.",
                )?;
                let turn = data.first().filter(|value| !value.is_null());
                if self.phase == Phase::Full {
                    require(
                        turn.is_some_and(|turn| {
                            turn.get("id") == Some(&Json::String(self.turn_id.clone()))
                                && turn.get("status") == Some(&self.native_state)
                                && turn.get("itemsView") == Some(&"full".into())
                                && turn.get("items").and_then(Json::as_array).is_some()
                        }),
                        "chat_collection_changed",
                        "The original full-turn page must still name the same terminal turn with complete items.",
                    )?;
                    self.append_items(turn.unwrap().get("items").unwrap().as_array().unwrap())?;
                    self.verify_next();
                } else if turn
                    .is_some_and(|turn| turn.get("id") == Some(&Json::String(self.turn_id.clone())))
                {
                    let status = turn.unwrap().get("status");
                    if self.phase == Phase::Search && status == Some(&"inProgress".into()) {
                        return Ok(false);
                    }
                    let terminal = status
                        .map(try_js_string)
                        .transpose()?
                        .is_some_and(|status| {
                            ["completed", "failed", "interrupted"]
                                .iter()
                                .any(|value| status == JsString::from(*value))
                        });
                    require(
                        terminal
                            && (self.phase == Phase::Search || status == Some(&self.native_state)),
                        "chat_collection_changed",
                        "The original turn must retain its terminal status through final verification.",
                    )?;
                    if self.phase == Phase::Search {
                        self.native_state = status.unwrap().clone();
                        self.found_cursor = self.cursor.clone();
                        self.phase = Phase::Items;
                        self.cursor = None;
                        self.cursors.clear();
                    } else {
                        self.phase = Phase::Done;
                    }
                } else {
                    let id = turn
                        .and_then(|turn| turn.get("id"))
                        .and_then(Json::as_js_string);
                    require(
                        id.is_some_and(|id| !self.turns.contains(id)) && next_cursor.is_some(),
                        "chat_collection_gap",
                        "The original turn is missing or its search contains repeated turns.",
                    )?;
                    self.turns.insert(id.unwrap().clone());
                    self.advance(next_cursor)?;
                }
            }
        }
        self.observations.insert(observation_id.clone());
        self.native_bytes += bytes;
        self.count += 1;
        self.last_observed_at = observed.get("observedAt").cloned();
        Ok(true)
    }
}
