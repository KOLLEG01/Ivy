//! The existing non-STRICT chat-bridge.sqlite journal. Call from its database
//! owner, outside the async reactor. The outer store supplies its sampled clock
//! and ISO timestamp; this layer performs no RPC or native dispatch.
use super::{Destination, Document, Pin, StoreResult, mutation};
use crate::schema::VERSION;
use ivy_contracts::{
    IvyError, IvyJson as Json, JsString,
    hash::hash_json,
    json::{canonical, number_from_string, object_entries, parse, try_js_string},
    limits::MANAGEMENT_FRAME_BYTES,
    operation::parse_operation_id_js,
};
use ivy_store::receipts::has_unresolved_outcome;
use rusqlite::{Connection, OptionalExtension, Params, params};
use std::{
    collections::{HashSet, VecDeque},
    path::Path,
};

const SCHEMA: &str = include_str!("local.sql");

#[derive(Clone, Copy)]
pub struct Limits {
    pub count: usize,
    pub bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            count: 10_000,
            bytes: 256 * 1024 * 1024,
        }
    }
}
pub struct LocalStore {
    db: Connection,
    pub limits: Limits,
    pub diagnostic_history_limits: Limits,
    pub change_version: u64,
    runtime_epoch: Option<JsString>,
    last_maintenance: i64,
}
fn encoded(value: &Json) -> StoreResult<String> {
    Ok(canonical(value, MANAGEMENT_FRAME_BYTES)?)
}
fn decoded(value: &str) -> StoreResult<Json> {
    // JSON.parse failures in persisted rows are ordinary errors at the original
    // boundary, not caller validation errors.
    parse(value, usize::MAX).map_err(|_| IvyError::internal().into())
}
fn require(ok: bool, code: &str, message: &str) -> StoreResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message).into())
    }
}
fn text(value: &Json, key: &str) -> StoreResult<String> {
    Ok(value.required(key)?.as_str()?)
}
fn at<'a>(value: &'a Json, keys: &[&str]) -> Option<&'a Json> {
    keys.iter().try_fold(value, |v, key| v.get(key))
}
fn is(value: Option<&Json>, expected: &str) -> bool {
    value == Some(&Json::from(expected))
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
fn terminal(value: &Json) -> bool {
    is(value.get("phase"), "succeeded") || is(value.get("phase"), "failed")
}
fn strings(value: &Json) -> HashSet<JsString> {
    let mut found = HashSet::new();
    let mut pending = vec![value];
    while let Some(v) = pending.pop() {
        match v {
            Json::String(v) => {
                found.insert(v.clone());
            }
            Json::Array(v) => pending.extend(v),
            Json::Object(v) => pending.extend(v.iter().map(|(_, v)| v)),
            _ => {}
        }
    }
    found
}
fn number(value: &Json) -> StoreResult<f64> {
    Ok(match value {
        Json::Number(v) => *v,
        Json::Null => 0.0,
        Json::Bool(v) => {
            if *v {
                1.0
            } else {
                0.0
            }
        }
        _ => number_from_string(&try_js_string(value)?),
    })
}

impl LocalStore {
    /// The owner creates the protected data directory. Pass None for an in-memory store.
    pub fn open(data_root: Option<&Path>) -> StoreResult<Self> {
        let db = match data_root {
            Some(root) => Connection::open(root.join("chat-bridge.sqlite"))?,
            None => Connection::open_in_memory()?,
        };
        db.execute_batch(SCHEMA)?;
        Ok(Self {
            db,
            limits: Limits::default(),
            diagnostic_history_limits: Limits {
                count: 10_000,
                bytes: 16 * 1024 * 1024,
            },
            change_version: 0,
            runtime_epoch: None,
            last_maintenance: 0,
        })
    }
    pub fn close(self) -> StoreResult<()> {
        self.db.close().map_err(|(_, e)| e.into())
    }
    fn rows<T>(
        &self,
        sql: &str,
        params: impl Params,
        f: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> StoreResult<Vec<T>> {
        Ok(self
            .db
            .prepare(sql)?
            .query_map(params, f)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    fn current(&self, key: &str) -> StoreResult<Vec<(String, Json)>> {
        self.rows("SELECT d.id,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key=?", [key], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .into_iter().map(|(id, value)| Ok((id, decoded(&value)?))).collect()
    }
    fn current_value(&self, id: &str) -> StoreResult<Option<Json>> {
        self.db.query_row("SELECT r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.id=?", [id], |r| r.get::<_, String>(0)).optional()?.map(|s| decoded(&s)).transpose()
    }
    fn set_marker(&self, key: &str, value: &Json) -> StoreResult<()> {
        self.db.execute("INSERT INTO markers VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json", params![key, encoded(value)?])?;
        Ok(())
    }
    fn marker(&self, key: &str) -> StoreResult<f64> {
        let raw = self
            .db
            .query_row("SELECT value_json FROM markers WHERE key=?", [key], |r| {
                r.get::<_, String>(0)
            })
            .optional()?;
        raw.map(|s| number(&decoded(&s)?)).unwrap_or(Ok(-1.0))
    }
    pub fn set_runtime_epoch(&mut self, epoch: JsString, now: i64) -> StoreResult<()> {
        require(
            !epoch.0.is_empty() && !epoch.0.contains(&58),
            "invalid_arguments",
            "Invalid runtime epoch.",
        )?;
        self.runtime_epoch = Some(epoch);
        self.expire_operations(now, false, None, None)
    }
    pub fn runtime_epoch(&self) -> Option<&JsString> {
        self.runtime_epoch.as_ref()
    }
    /// The outer store first obtains system.status when runtime_epoch is absent.
    pub fn validate_operation_id(
        &mut self,
        value: &JsString,
        now: i64,
        retained: bool,
    ) -> StoreResult<()> {
        self.expire_operations(now, false, None, None)?;
        let identity = parse_operation_id_js(value)?;
        require(
            self.runtime_epoch.as_ref() == Some(&identity.runtime_epoch)
                && (retained
                    || (identity.issued_at_unix_ms as f64
                        > self.marker("operations.expiredBefore")?
                        && identity.issued_at_unix_ms as f64 <= now as f64 + 60_000.0)),
            "operation_expired",
            "The ChatBridge operation belongs to another runtime or predates the receipt ring.",
        )
    }
    pub fn diagnostic_notice_state(&self, scope: &str) -> StoreResult<Json> {
        let row = self
            .db
            .query_row(
                "SELECT value_json FROM markers WHERE key=?",
                [format!("diagnostic-notices:{scope}")],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        row.map(|s| decoded(&s))
            .unwrap_or_else(|| Ok(Json::Object(vec![])))
    }
    pub fn save_diagnostic_notice_state(&self, scope: &str, value: &mut Json) -> StoreResult<()> {
        let mut count = object_entries(value).len();
        let mut bytes = encoded(value)?.len();
        let mut completed = object_entries(value)
            .into_iter()
            .filter(|(_, e)| {
                !e.get("active").is_some_and(truthy)
                    && e.get("resolvedAt") != Some(&Json::Null)
                    && at(e, &["alert", "confirmed"]).is_some_and(truthy)
                    && at(e, &["recovery", "confirmed"]).is_some_and(truthy)
            })
            .map(|(key, entry)| (key.clone(), entry.clone()))
            .collect::<Vec<_>>();
        completed.sort_by(|(_, a), (_, b)| {
            let a = a
                .get("resolvedAt")
                .and_then(Json::as_f64)
                .unwrap_or(f64::NAN);
            let b = b
                .get("resolvedAt")
                .and_then(Json::as_f64)
                .unwrap_or(f64::NAN);
            a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
        });
        for (key, entry) in completed {
            if count <= self.diagnostic_history_limits.count
                && bytes <= self.diagnostic_history_limits.bytes
            {
                break;
            }
            if let Json::Object(pairs) = value {
                pairs.retain(|(k, _)| k != &key);
            }
            count -= 1;
            bytes = if count == 0 {
                2
            } else {
                bytes - encoded(&Json::object([(key, entry)]))?.len() + 1
            };
        }
        self.set_marker(&format!("diagnostic-notices:{scope}"), value)
    }
    pub fn remember_notice_input(&self, input_id: &str, now: i64) -> StoreResult<()> {
        let id = mutation(input_id, "native-user-message")?;
        if !self.is_notice_input(&id, now)? {
            self.set_marker(
                &format!("notice-input:{id}"),
                &Json::Number(now as f64 + 7.0 * 86_400_000.0),
            )?;
        }
        Ok(())
    }
    pub fn is_notice_input(&self, client_id: &str, now: i64) -> StoreResult<bool> {
        Ok(self.marker(&format!("notice-input:{client_id}"))? > now as f64)
    }
    pub fn remember_notice_delivery(&self, reply_id: &str, now: i64) -> StoreResult<()> {
        self.db.execute(
            "INSERT INTO markers VALUES(?,?) ON CONFLICT(key) DO NOTHING",
            params![
                format!("notice-delivered:{reply_id}"),
                encoded(&Json::Number(now as f64))?
            ],
        )?;
        Ok(())
    }
    fn protected_pins(&self) -> StoreResult<HashSet<String>> {
        let mut result = HashSet::new();
        for (id, main) in self.current("chat-bridge/main")? {
            let Some(pending) = main.get("pendingAction").filter(|v| truthy(v)) else {
                continue;
            };
            let operation_id = text(pending, "objectId")?;
            result.insert(format!(
                "{}:{}",
                operation_id,
                pending.required("revision")?.as_f64().unwrap_or(0.0)
            ));
            let raw = self.db.query_row("SELECT r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.id=? AND d.key='chat-bridge/operation'", [&operation_id], |r| r.get::<_, String>(0)).optional()?;
            if let Some(raw) = raw {
                let value = decoded(&raw)?;
                if is(at(&value, &["request", "action"]), "createMain") {
                    let request = value.required("request")?;
                    if let Some(rev) = request.get("expectedMainRevision").filter(|v| !v.is_null())
                    {
                        result.insert(format!("{id}:{}", rev.as_f64().unwrap_or(0.0)));
                    }
                    if let Some(resume) = request.get("resumeMain").filter(|v| truthy(v)) {
                        result.insert(format!(
                            "{}:{}",
                            text(resume, "objectId")?,
                            resume.required("revision")?.as_f64().unwrap_or(0.0)
                        ));
                    }
                }
            }
        }
        Ok(result)
    }
    fn technical_tree(&self, root: &str, include_root: bool) -> StoreResult<Vec<String>> {
        let mut selected = HashSet::new();
        let mut result = vec![];
        if include_root {
            selected.insert(root.to_owned());
            result.push(root.to_owned());
        }
        let mut queue = VecDeque::from([root.to_owned()]);
        while let Some(parent) = queue.pop_front() {
            for id in self.rows(
                "SELECT id FROM documents WHERE parent_id=?",
                [&parent],
                |r| r.get::<_, String>(0),
            )? {
                if selected.insert(id.clone()) {
                    result.push(id.clone());
                    queue.push_back(id);
                }
            }
        }
        Ok(result)
    }
    fn delete_technical_tree(&self, root: &str, include_root: bool) -> StoreResult<()> {
        let ids = self.technical_tree(root, include_root)?;
        for parent in std::iter::once(root).chain(ids.iter().map(String::as_str)) {
            self.db
                .execute("DELETE FROM blobs WHERE parent_id=?", [parent])?;
        }
        for id in ids.iter().rev() {
            self.db.execute("DELETE FROM revisions WHERE id=?", [id])?;
            self.db.execute("DELETE FROM documents WHERE id=?", [id])?;
        }
        Ok(())
    }
    fn prune_historical_revisions(
        &self,
        protected: &HashSet<String>,
        id: Option<&str>,
    ) -> StoreResult<()> {
        let rows = self.rows("SELECT r.id,r.revision,d.key,d.current_revision FROM revisions r JOIN documents d ON d.id=r.id WHERE r.revision<>d.current_revision AND (? IS NULL OR r.id=?)", params![id, id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?)))?;
        for (id, revision, key, current) in rows {
            if !(key == "chat-bridge/collection" && revision > current - 32)
                && !protected.contains(&format!("{id}:{revision}"))
            {
                self.db.execute(
                    "DELETE FROM revisions WHERE id=? AND revision=?",
                    params![id, revision],
                )?;
            }
        }
        Ok(())
    }
    fn needs_room(&self) -> StoreResult<bool> {
        let (rows, bytes) = self.db.query_row("SELECT (SELECT COUNT(*) FROM revisions)+(SELECT COUNT(*) FROM blobs),COALESCE((SELECT SUM(length(CAST(value_json AS BLOB))) FROM revisions),0)+COALESCE((SELECT SUM(length(bytes)) FROM blobs),0)+(SELECT COUNT(*)*1048576 FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='chat-bridge/operation' AND json_extract(r.value_json,'$.phase') NOT IN ('succeeded','failed'))", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        Ok(rows as u64 > self.limits.count as u64 || bytes as u64 > self.limits.bytes as u64)
    }
    fn make_room(&mut self, id: &str, incoming: &Json, now: i64) -> StoreResult<()> {
        if !self.needs_room()? {
            return Ok(());
        }
        self.expire_operations(now, true, Some(id), Some(incoming))?;
        require(
            !self.needs_room()?,
            "capacity_exceeded",
            "ChatBridge journal capacity is occupied by live or unresolved work.",
        )
    }
    pub fn confirm_offer(&self, offer: &str, replies: &[String]) -> StoreResult<()> {
        self.db.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            for reply in replies {
                self.db.execute(
                    "INSERT OR IGNORE INTO offer_acknowledgements VALUES(?,?)",
                    params![offer, reply],
                )?;
            }
            self.db.execute_batch("COMMIT")?;
            Ok(())
        })();
        if result.is_err() {
            self.db.execute_batch("ROLLBACK")?;
        }
        result
    }
    pub fn expire_operations(
        &mut self,
        now: i64,
        force: bool,
        protected_id: Option<&str>,
        incoming: Option<&Json>,
    ) -> StoreResult<()> {
        if !force && now as f64 - (self.last_maintenance as f64) < 60_000.0 && !self.needs_room()? {
            return Ok(());
        }
        self.db.execute("DELETE FROM workflow_cleanup WHERE NOT EXISTS(SELECT 1 FROM documents WHERE parent_id=workflow_cleanup.root_id) AND NOT EXISTS(SELECT 1 FROM blobs WHERE parent_id=workflow_cleanup.root_id)", [])?;
        let mut incoming_ids = incoming.map(strings).unwrap_or_default();
        if let Some(id) = protected_id {
            incoming_ids.insert(id.into());
        }
        let protected = self.protected_pins()?;
        let protected_objects = protected
            .iter()
            .filter_map(|s| s.rsplit_once(':').map(|(id, _)| id.to_owned()))
            .collect::<HashSet<_>>();
        let mut queued_inputs = HashSet::new();
        let mut queued_notices = HashSet::new();
        for (_, main) in self.current("chat-bridge/main")? {
            if let Some(queue) = main.get("queue").and_then(Json::as_array) {
                for ticket in queue {
                    if let Some(id) = ticket.get("inputId").and_then(Json::as_js_string) {
                        queued_inputs.insert(id.clone());
                    }
                    if let Some(identity) = ticket.get("identity").filter(|v| truthy(v)) {
                        queued_notices.insert(hash_json(&Json::Array(vec![
                            identity.required("senderPrincipalId")?.clone(),
                            identity.required("messageId")?.clone(),
                        ]))?);
                    }
                }
            }
        }
        let rows = self.rows("SELECT d.id,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='chat-bridge/operation' ORDER BY json_extract(r.value_json,'$.updatedAt'),d.rowid", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .into_iter().map(|(id, s)| Ok((id, decoded(&s)?))).collect::<StoreResult<Vec<_>>>()?;
        let pending_offers = rows
            .iter()
            .filter(|(_, v)| !terminal(v) && is(at(v, &["request", "action"]), "acknowledge"))
            .filter_map(|(_, v)| {
                at(v, &["request", "offerId"])
                    .and_then(Json::as_js_string)
                    .cloned()
            })
            .collect::<HashSet<_>>();
        let own_transaction = self.db.is_autocommit();
        if own_transaction {
            self.db.execute_batch("BEGIN IMMEDIATE")?;
        }
        let result = (|| {
            let mut pressure = self.needs_room()?;
            for (id, value) in rows {
                if !pressure {
                    break;
                }
                let mut outcome_fields = Json::Object(vec![]);
                for key in ["error", "nativeCall"] {
                    if let Some(v) = value.get(key) {
                        outcome_fields.insert(key, v.clone());
                    }
                }
                if !terminal(&value)
                    || protected_objects.contains(&id)
                    || incoming_ids.contains(&JsString::from(id.as_str()))
                    || has_unresolved_outcome(&outcome_fields)?
                {
                    continue;
                }
                let Some(operation_id) = value.get("operationId").and_then(Json::as_js_string)
                else {
                    continue;
                };
                let Ok(identity) = parse_operation_id_js(operation_id) else {
                    continue;
                };
                if at(&value, &["outcome", "input", "object", "objectId"])
                    .and_then(Json::as_js_string)
                    .is_some_and(|id| queued_inputs.contains(id))
                {
                    continue;
                }
                if is(at(&value, &["request", "action"]), "notify")
                    && queued_notices.contains(&hash_json(&Json::Array(vec![
                        value
                            .get("callerPrincipalId")
                            .cloned()
                            .unwrap_or(Json::Null),
                        value.required("operationId")?.clone(),
                    ]))?)
                {
                    continue;
                }
                let notified_reply = if is(at(&value, &["outcome", "action"]), "notify") {
                    Some(
                        at(&value, &["outcome", "reply", "object", "objectId"])
                            .ok_or_else(IvyError::internal)?
                            .as_str()?,
                    )
                } else {
                    None
                };
                if let Some(reply) = &notified_reply {
                    if self.marker(&format!("notice-delivered:{reply}"))? < 0.0 {
                        continue;
                    }
                }
                let offer = self.db.query_row("SELECT d.id,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='chat-bridge/offer' AND d.name=? AND d.parent_id IS (SELECT parent_id FROM documents WHERE id=?)", params![format!("Chat offer {id}"), id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).optional()?;
                if let Some((offer_id, raw)) = offer {
                    if pending_offers.contains(&JsString::from(offer_id.as_str()))
                        || incoming_ids.contains(&JsString::from(offer_id.as_str()))
                    {
                        continue;
                    }
                    let offer_value = decoded(&raw)?;
                    let mut unconfirmed = false;
                    if let Some(replies) = offer_value.get("replies").and_then(Json::as_array) {
                        for reply in replies {
                            if self.db.query_row("SELECT 1 FROM offer_acknowledgements WHERE offer_id=? AND reply_id=?", params![offer_id, text(reply, "objectId")?], |_| Ok(())).optional()?.is_none() { unconfirmed = true; break; }
                        }
                    }
                    if unconfirmed {
                        continue;
                    }
                }
                for (key, name) in [
                    ("chat-bridge/offer", "Chat offer "),
                    ("chat-bridge/acknowledgement", "Chat acknowledgement "),
                ] {
                    let receipt = self.db.query_row("SELECT id FROM documents WHERE key=? AND name=? AND parent_id IS (SELECT parent_id FROM documents WHERE id=?)", params![key, format!("{name}{id}"), id], |r| r.get::<_, String>(0)).optional()?;
                    if let Some(receipt) = receipt {
                        self.db.execute(
                            "DELETE FROM offer_acknowledgements WHERE offer_id=?",
                            [&receipt],
                        )?;
                        self.delete_technical_tree(&receipt, true)?;
                    }
                }
                if let Some(reply) = notified_reply {
                    self.db.execute(
                        "DELETE FROM markers WHERE key=?",
                        [format!("notice-delivered:{reply}")],
                    )?;
                }
                self.db.execute(
                    "INSERT OR IGNORE INTO retired_operations VALUES(?,?)",
                    params![
                        String::from_utf16_lossy(&operation_id.0),
                        identity.issued_at_unix_ms as i64
                    ],
                )?;
                self.delete_technical_tree(&id, true)?;
                self.set_marker(
                    "operations.expiredBefore",
                    &Json::Number(
                        self.marker("operations.expiredBefore")?
                            .max(identity.issued_at_unix_ms as f64),
                    ),
                )?;
                pressure = self.needs_room()?;
            }
            self.db.execute(
                "DELETE FROM retired_operations WHERE issued_at<?",
                [self.marker("operations.expiredBefore")?],
            )?;
            for root in self.rows(
                "SELECT root_id FROM workflow_cleanup ORDER BY delete_after,rowid",
                [],
                |r| r.get::<_, String>(0),
            )? {
                if !pressure {
                    break;
                }
                let ids = self.technical_tree(&root, false)?;
                if queued_inputs.contains(&JsString::from(root.as_str()))
                    || std::iter::once(&root).chain(ids.iter()).any(|id| {
                        protected_objects.contains(id)
                            || incoming_ids.contains(&JsString::from(id.as_str()))
                    })
                {
                    continue;
                }
                let mut unresolved = false;
                for id in ids {
                    if let Some(value) = self.current_value(&id)? {
                        if has_unresolved_outcome(&value)? {
                            unresolved = true;
                            break;
                        }
                    }
                }
                if unresolved {
                    continue;
                }
                self.delete_technical_tree(&root, false)?;
                self.db
                    .execute("DELETE FROM workflow_cleanup WHERE root_id=?", [&root])?;
                pressure = self.needs_room()?;
            }
            self.db.execute("DELETE FROM markers WHERE key LIKE 'notice-input:%' AND CAST(value_json AS INTEGER)<=?", [now])?;
            self.prune_historical_revisions(&protected, None)?;
            if own_transaction {
                self.db.execute_batch("COMMIT")?;
            }
            self.last_maintenance = now;
            Ok(())
        })();
        if result.is_err() && own_transaction {
            self.db.execute_batch("ROLLBACK")?;
        }
        result
    }
    pub fn document(
        &self,
        key: &str,
        id: &str,
        requested: Option<i64>,
        parent_id: Option<&str>,
    ) -> StoreResult<Document> {
        let head = self.db.query_row("SELECT name,parent_id,current_revision,created_at,updated_at FROM documents WHERE id=? AND key=?", params![id, key], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?))).optional()?;
        require(
            head.as_ref().is_some_and(|h| h.1.as_deref() == parent_id),
            "chat_scope_mismatch",
            "The selected local ChatBridge record is outside the active scope.",
        )?;
        let (name, _, current, created_at, updated_at) = head.unwrap();
        let requested = requested.unwrap_or(current);
        let read = |revision| {
            self.db.query_row("SELECT revision,value_json,created_at FROM revisions WHERE id=? AND revision=?", params![id, revision], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))).optional()
        };
        let mut row = read(requested)?;
        if row.is_none() && requested < current {
            row = read(current)?;
        }
        require(
            row.is_some(),
            "chat_evidence_mismatch",
            "The selected local ChatBridge revision is unavailable.",
        )?;
        let (revision, raw, at) = row.unwrap();
        let value = decoded(&raw)?;
        let metadata = Json::object([
            ("id", id.into()),
            ("parentId", parent_id.map(Json::from).unwrap_or(Json::Null)),
            ("ownerObjectId", Json::Null),
            ("name", name.into()),
            ("path", "".into()),
            ("position", Json::Number(0.0)),
            ("icon", Json::Null),
            ("contractKey", key.into()),
            ("currentRevision", Json::Number(current as f64)),
            ("contractVersion", VERSION.into()),
            ("archivedAt", Json::Null),
            ("effectivelyArchived", false.into()),
            ("createdAt", created_at.into()),
            ("updatedAt", updated_at.into()),
        ]);
        let revision_metadata = Json::object([
            ("objectId", id.into()),
            ("revision", Json::Number(revision as f64)),
            ("contractVersion", VERSION.into()),
            ("contentHash", hash_json(&value)?.into()),
            ("mediaType", "application/json".into()),
            ("byteLength", Json::Number(raw.len() as f64)),
            ("createdAt", at.into()),
            ("references", Json::Object(vec![])),
        ]);
        Ok(Document {
            key: key.into(),
            pin: Pin {
                object_id: id.into(),
                revision,
            },
            value,
            metadata,
            revision: revision_metadata,
        })
    }
    pub fn named(
        &self,
        key: &str,
        name: &str,
        parent: Option<&str>,
    ) -> StoreResult<Option<Document>> {
        let id = self
            .db
            .query_row(
                "SELECT id FROM documents WHERE key=? AND name=? AND parent_id IS ?",
                params![key, name, parent],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        id.map(|id| self.document(key, &id, None, parent))
            .transpose()
    }
    pub fn write(
        &mut self,
        key: &str,
        value: &Json,
        destination: Destination<'_>,
        now: i64,
        iso: &str,
    ) -> StoreResult<Pin> {
        let encoded = encoded(value)?;
        match destination {
            Destination::Create { parent_id, name } => {
                if let Some(existing) = self.named(key, name, parent_id)? {
                    require(
                        existing.pin.revision == 1
                            && hash_json(&existing.value)? == hash_json(value)?,
                        "revision_conflict",
                        "The local ChatBridge identity differs.",
                    )?;
                    return Ok(existing.pin);
                }
                let id = uuid::Uuid::new_v4().to_string();
                self.db.execute_batch("BEGIN IMMEDIATE")?;
                let result: StoreResult<()> = (|| {
                    self.db.execute(
                        "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                        params![id, key, name, parent_id, 1, iso, iso],
                    )?;
                    self.db.execute(
                        "INSERT INTO revisions VALUES(?,?,?,?)",
                        params![id, 1, encoded, iso],
                    )?;
                    self.make_room(&id, value, now)?;
                    self.db.execute_batch("COMMIT")?;
                    Ok(())
                })();
                if let Err(error) = result {
                    self.db.execute_batch("ROLLBACK")?;
                    if let Some(winner) = self.named(key, name, parent_id)? {
                        require(
                            hash_json(&winner.value)? == hash_json(value)?,
                            "revision_conflict",
                            "The local ChatBridge identity already exists.",
                        )?;
                        return Ok(winner.pin);
                    }
                    return Err(error);
                }
                self.change_version += 1;
                Ok(Pin {
                    object_id: id,
                    revision: 1,
                })
            }
            Destination::Update {
                object_id,
                expected_revision,
            } => {
                let next = expected_revision + 1;
                self.db.execute_batch("BEGIN IMMEDIATE")?;
                let result: StoreResult<()> = (|| {
                    let changed = self.db.execute("UPDATE documents SET current_revision=?,updated_at=? WHERE id=? AND key=? AND current_revision=?", params![next, iso, object_id, key, expected_revision])?;
                    require(
                        changed == 1,
                        "revision_conflict",
                        "The local ChatBridge row changed.",
                    )?;
                    self.db.execute(
                        "INSERT INTO revisions VALUES(?,?,?,?)",
                        params![object_id, next, encoded, iso],
                    )?;
                    self.prune_historical_revisions(&self.protected_pins()?, Some(object_id))?;
                    self.make_room(object_id, value, now)?;
                    self.db.execute_batch("COMMIT")?;
                    Ok(())
                })();
                if result.is_err() {
                    self.db.execute_batch("ROLLBACK")?;
                }
                result?;
                self.change_version += 1;
                Ok(Pin {
                    object_id: object_id.into(),
                    revision: next,
                })
            }
        }
    }
    /// Called only after the public terminal Input write is confirmed by Hive.
    pub fn completed_input(&mut self, id: &str, finished_at: f64, now: i64) -> StoreResult<()> {
        self.db.execute(
            "INSERT INTO workflow_cleanup VALUES(?,?) ON CONFLICT(root_id) DO NOTHING",
            params![id, finished_at],
        )?;
        self.expire_operations(now, true, None, None)
    }
    pub fn local_info(&self, id: &str) -> StoreResult<Option<(String, Option<String>)>> {
        for table in ["documents", "blobs"] {
            let row = self
                .db
                .query_row(
                    &format!("SELECT key,parent_id FROM {table} WHERE id=?"),
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if row.is_some() {
                return Ok(row);
            }
        }
        Ok(None)
    }
    pub fn blob(&self, key: &str, pin: &Pin, parent: &str) -> StoreResult<Vec<u8>> {
        require(
            pin.revision == 1,
            "chat_evidence_mismatch",
            "Local evidence blobs have one immutable revision.",
        )?;
        let row = self
            .db
            .query_row(
                "SELECT bytes FROM blobs WHERE id=? AND key=? AND parent_id=?",
                params![pin.object_id, key, parent],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        require(
            row.is_some(),
            "chat_evidence_mismatch",
            "The local evidence blob is unavailable.",
        )?;
        Ok(row.unwrap())
    }
    pub fn save_blob(
        &mut self,
        key: &str,
        name: &str,
        parent: &str,
        bytes: &[u8],
        now: i64,
    ) -> StoreResult<Pin> {
        self.expire_operations(now, false, None, None)?;
        let existing = self
            .db
            .query_row(
                "SELECT id,bytes FROM blobs WHERE key=? AND name=? AND parent_id=?",
                params![key, name, parent],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        if let Some((id, original)) = existing {
            require(
                original == bytes,
                "revision_conflict",
                "The local evidence identity differs.",
            )?;
            return Ok(Pin {
                object_id: id,
                revision: 1,
            });
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.db.execute_batch("BEGIN IMMEDIATE")?;
        let result: StoreResult<()> = (|| {
            self.db.execute(
                "INSERT INTO blobs VALUES(?,?,?,?,?)",
                params![id, key, name, parent, bytes],
            )?;
            self.make_room(&id, &Json::from(parent), now)?;
            self.db.execute_batch("COMMIT")?;
            Ok(())
        })();
        if result.is_err() {
            self.db.execute_batch("ROLLBACK")?;
        }
        result?;
        Ok(Pin {
            object_id: id,
            revision: 1,
        })
    }
}
