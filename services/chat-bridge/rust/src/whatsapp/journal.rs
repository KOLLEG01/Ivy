//! WhatsApp journal policy over the ordered writer. Keep the legacy SQL rows,
//! compact tombstones and ordinary JSON identities readable across rollback.
use super::{
    identity::{identity, output_id, output_parts},
    journal_io::{JournalIo, StorageUnavailable},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    json::{parse, stringify},
};
use rusqlite::{OptionalExtension, types::Value};
use std::collections::HashSet;

impl From<StorageUnavailable> for IvyError {
    fn from(_: StorageUnavailable) -> Self {
        Self::new("storage_unavailable", "WhatsApp transport storage failed.")
    }
}
fn sql<T>(result: rusqlite::Result<T>) -> IvyResult<T> {
    result.map_err(|_| StorageUnavailable.into())
}
fn text(value: impl Into<String>) -> Value {
    Value::Text(value.into())
}
fn conflict(message: &str) -> IvyError {
    IvyError::new("whatsui_identity_conflict", message)
}
fn encoded(value: &IvyJson) -> IvyResult<String> {
    stringify(value, usize::MAX)
}

#[derive(Debug, PartialEq)]
pub struct Inbox {
    pub id: String,
    pub sender: String,
    pub jid: String,
    pub message: String,
    pub state: String,
    pub request: Option<String>,
    pub received: i64,
}
#[derive(Debug, PartialEq)]
pub struct Outgoing {
    pub id: String,
    pub jid: String,
    pub text: String,
    pub state: String,
    pub sequence: i64,
}
#[derive(Clone, Copy)]
pub struct HistoryLimits {
    pub count: i64,
    pub bytes: i64,
}
impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            count: 10_000,
            bytes: 64 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy)]
pub enum OutputState {
    Sending,
    Sent,
    Unknown,
}

impl JournalIo {
    pub fn get(&self, key: &str) -> IvyResult<IvyJson> {
        let raw: Option<String> = sql(self
            .connection()
            .query_row("SELECT value FROM kv WHERE key=?", [key], |r| r.get(0))
            .optional())?;
        raw.map(|s| parse(&s, usize::MAX))
            .unwrap_or(Ok(IvyJson::Null))
    }
    pub fn set(&mut self, key: &str, value: &IvyJson) -> IvyResult<()> {
        let json = encoded(value)?;
        let prior: Option<String> = sql(self
            .connection()
            .query_row("SELECT value FROM kv WHERE key=?", [key], |r| r.get(0))
            .optional())?;
        if prior.as_deref() == Some(&json) {
            return Ok(());
        }
        if *value == IvyJson::Null {
            self.write("DELETE FROM kv WHERE key=?", vec![text(key)])?;
        } else {
            self.write(
                "INSERT INTO kv VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                vec![text(key), text(json)],
            )?;
        }
        Ok(())
    }
    pub fn accept(
        &mut self,
        id: &str,
        sender: &str,
        jid: &str,
        message: &str,
        received: i64,
    ) -> IvyResult<()> {
        let retired: Option<String> = sql(self
            .connection()
            .query_row(
                "SELECT fingerprint FROM retired_inbox WHERE id=?",
                [id],
                |r| r.get(0),
            )
            .optional())?;
        let prior: Option<(String, String, String)> = sql(self
            .connection()
            .query_row(
                "SELECT sender,jid,message FROM inbox WHERE id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional())?;
        let fingerprint = identity(&[sender.into(), jid.into(), message.into()])?;
        if let Some((old_sender, old_jid, old_message)) = prior {
            if old_sender != sender
                || old_jid != jid
                || !match retired {
                    Some(hash) => hash == fingerprint,
                    None => old_message == message || old_message.is_empty(),
                }
            {
                return Err(conflict("The original WhatsApp message changed."));
            }
            return Ok(());
        }
        if retired.is_some() {
            return Ok(());
        }
        self.try_transaction(|journal| -> IvyResult<()> {
            journal.write(
                "INSERT INTO inbox(id,sender,jid,message,received) VALUES(?,?,?,?,?)",
                vec![
                    text(id),
                    text(sender),
                    text(jid),
                    text(message),
                    Value::Integer(received),
                ],
            )?;
            journal.write(
                "INSERT INTO retired_inbox VALUES(?,?,0)",
                vec![text(id), text(fingerprint)],
            )?;
            Ok(())
        })
    }
    pub fn pending(&self) -> IvyResult<Vec<Inbox>> {
        let mut query = sql(self.connection().prepare("SELECT id,sender,jid,message,state,request,received FROM inbox WHERE state='pending' ORDER BY received,id LIMIT 32"))?;
        sql(sql(query.query_map([], |r| {
            Ok(Inbox {
                id: r.get(0)?,
                sender: r.get(1)?,
                jid: r.get(2)?,
                message: r.get(3)?,
                state: r.get(4)?,
                request: r.get(5)?,
                received: r.get(6)?,
            })
        }))?
        .collect())
    }
    pub fn save_request(&mut self, id: &str, request: &IvyJson) -> IvyResult<()> {
        self.write(
            "UPDATE inbox SET request=? WHERE id=? AND request IS NULL",
            vec![text(encoded(request)?), text(id)],
        )?;
        Ok(())
    }
    pub fn rebind_request(
        &mut self,
        id: &str,
        previous: &IvyJson,
        next: &IvyJson,
    ) -> IvyResult<()> {
        let raw: Option<Option<String>> = sql(self
            .connection()
            .query_row(
                "SELECT request FROM inbox WHERE id=? AND state='pending'",
                [id],
                |r| r.get(0),
            )
            .optional())?;
        if raw.flatten().as_deref() != Some(&encoded(previous)?) {
            return Err(conflict(
                "Only the original pending input can adopt a recovered Main binding.",
            ));
        }
        self.write(
            "UPDATE inbox SET request=? WHERE id=? AND state='pending'",
            vec![text(encoded(next)?), text(id)],
        )?;
        Ok(())
    }
    pub fn finish(&mut self, id: &str) -> IvyResult<()> {
        self.write("UPDATE inbox SET state='done',request=NULL,message=CASE WHEN EXISTS(SELECT 1 FROM kv WHERE key='inbox-content:'||inbox.id) THEN '' ELSE message END WHERE id=?",vec![text(id)])?;
        for prefix in [
            "payload:",
            "transcript:",
            "direct-input:",
            "direct-request:",
            "native:",
            "native:input:",
            "interaction:",
            "interaction:input:",
            "steer-binding:",
            "recover-main:",
        ] {
            self.set(&format!("{prefix}{id}"), &IvyJson::Null)?;
        }
        Ok(())
    }
    /// now_ms is supplied by the runtime clock for each admission.
    pub fn enqueue(
        &mut self,
        key: &JsString,
        jid: &str,
        body: &JsString,
        automatic: bool,
        now_ms: i64,
    ) -> IvyResult<Vec<String>> {
        let mut ids = Vec::new();
        for part in output_parts(key, body)? {
            let id = part.id;
            ids.push(id.clone());
            let retired: Option<String> = sql(self
                .connection()
                .query_row(
                    "SELECT fingerprint FROM retired_outgoing WHERE id=?",
                    [&id],
                    |r| r.get(0),
                )
                .optional())?;
            if let Some(retired) = retired {
                if retired != identity(&[jid.into(), IvyJson::String(part.text)])? {
                    return Err(conflict(
                        "A delivered reply cannot change its recipient or text.",
                    ));
                }
                continue;
            }
            let prior: Option<(String, String)> = sql(self
                .connection()
                .query_row("SELECT jid,text FROM outgoing WHERE id=?", [&id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional())?;
            if let Some((old_jid, old_text)) = &prior {
                if old_jid != jid || JsString::from(old_text.as_str()) != part.text {
                    return Err(conflict(
                        "A saved reply cannot change its recipient or text.",
                    ));
                }
            }
            if automatic {
                self.set(&format!("out-automatic:{id}"), &IvyJson::Bool(true))?;
            }
            if prior.is_none() {
                self.write(
                    "INSERT OR IGNORE INTO outgoing(id,jid,text) VALUES(?,?,?)",
                    vec![
                        text(&id),
                        text(jid),
                        text(String::from_utf16_lossy(&part.text.0)),
                    ],
                )?;
                self.set(
                    &format!("out-created:{id}"),
                    &IvyJson::Number(now_ms as f64),
                )?;
            }
        }
        Ok(ids)
    }
    pub fn has_output(&self, key: &JsString) -> IvyResult<bool> {
        let id = output_id(key, 0)?;
        Ok(sql(self.connection().query_row("SELECT 1 FROM outgoing WHERE id=? UNION ALL SELECT 1 FROM retired_outgoing WHERE id=? LIMIT 1", [&id,&id], |_| Ok(())).optional())?.is_some())
    }
    pub fn notice_text(&self, key: &JsString) -> IvyResult<Option<String>> {
        let id = output_id(key, 0)?;
        if sql(self
            .connection()
            .query_row("SELECT 1 FROM retired_outgoing WHERE id=?", [&id], |_| {
                Ok(())
            })
            .optional())?
        .is_some()
        {
            return Ok(None);
        }
        sql(self
            .connection()
            .query_row("SELECT text FROM outgoing WHERE id=?", [&id], |r| r.get(0))
            .optional())
    }
    pub fn outgoing(&self) -> IvyResult<Vec<Outgoing>> {
        let mut query = sql(self.connection().prepare("SELECT id,jid,text,state,sequence FROM outgoing WHERE state<>'sent' ORDER BY sequence LIMIT 16"))?;
        sql(sql(query.query_map([], |r| {
            Ok(Outgoing {
                id: r.get(0)?,
                jid: r.get(1)?,
                text: r.get(2)?,
                state: r.get(3)?,
                sequence: r.get(4)?,
            })
        }))?
        .collect())
    }
    pub fn mark(&mut self, id: &str, state: OutputState) -> IvyResult<()> {
        let state = match state {
            OutputState::Sending => "sending",
            OutputState::Sent => "sent",
            OutputState::Unknown => "unknown",
        };
        self.write(
            "UPDATE outgoing SET state=? WHERE id=? AND state<>'sent'",
            vec![text(state), text(id)],
        )?;
        Ok(())
    }
    pub fn sent(&self, ids: &[String]) -> IvyResult<bool> {
        for id in ids {
            if sql(self.connection().query_row("SELECT 1 FROM outgoing WHERE id=? AND state='sent' UNION ALL SELECT 1 FROM retired_outgoing WHERE id=? LIMIT 1", [id,id], |_| Ok(())).optional())?.is_none() { return Ok(false); }
        }
        Ok(true)
    }
    fn kv_rows(&self, statement: &str) -> IvyResult<Vec<(String, IvyJson)>> {
        let mut query = sql(self.connection().prepare(statement))?;
        let rows: Vec<(String, String)> =
            sql(sql(query.query_map([], |r| Ok((r.get(0)?, r.get(1)?))))?.collect())?;
        rows.into_iter()
            .map(|(k, v)| Ok((k, parse(&v, usize::MAX)?)))
            .collect()
    }
    pub fn prune(&mut self, limits: HistoryLimits) -> IvyResult<()> {
        let mut protected_messages = Vec::new();
        let mut protected_outputs = HashSet::new();
        for (key, cursor) in self.kv_rows("SELECT key,value FROM kv WHERE key LIKE 'listener:%'")? {
            for field in ["lastTurn", "head"] {
                if let Some(IvyJson::String(turn)) = cursor.get(field) {
                    let turn = turn.to_utf8()?;
                    if !turn.is_empty() {
                        protected_messages
                            .push(format!("message:{}:{turn}:", &key["listener:".len()..]));
                    }
                }
            }
        }
        for (key, value) in self.kv_rows("SELECT key,value FROM kv WHERE key LIKE 'message:%'")? {
            if protected_messages.iter().any(|p| key.starts_with(p)) {
                protected_outputs.extend(string_ids(value.required("ids")?)?);
            }
        }
        self.try_transaction(|journal| -> IvyResult<()> {
            for (key,value) in journal.kv_rows("SELECT key,value FROM kv WHERE key LIKE 'turn-delivered:%' AND json_type(value)='array'")? {
                let ids = string_ids(&value)?;
                if journal.sent(&ids)? { journal.set(&key,&IvyJson::Bool(true))?; }
                else { protected_outputs.extend(ids); }
            }
            let (mut count,mut bytes): (i64,i64) = sql(journal.connection().query_row("SELECT (SELECT COUNT(*) FROM inbox WHERE id NOT IN (SELECT id FROM retired_inbox WHERE retired=1))+(SELECT COUNT(*) FROM outgoing WHERE id NOT IN (SELECT id FROM retired_outgoing))+(SELECT COUNT(*) FROM kv WHERE key LIKE 'message:%') AS count,COALESCE((SELECT SUM(length(CAST(message AS BLOB))) FROM inbox),0)+COALESCE((SELECT SUM(length(CAST(text AS BLOB))) FROM outgoing),0)+COALESCE((SELECT SUM(length(CAST(value AS BLOB))) FROM kv WHERE key LIKE 'message:%'),0) AS bytes",[],|r|Ok((r.get(0)?,r.get(1)?))))?;
            if count <= limits.count && bytes <= limits.bytes { return Ok(()); }
            let mut candidates = Vec::new();
            for (kind,statement) in [
                (0,"SELECT id,received,length(CAST(message AS BLOB)) FROM inbox WHERE state='done' AND id NOT IN (SELECT id FROM retired_inbox WHERE retired=1)"),
                (1,"SELECT o.id,COALESCE(CAST(k.value AS INTEGER),0),length(CAST(o.text AS BLOB)) FROM outgoing o LEFT JOIN kv k ON k.key='out-created:'||o.id WHERE o.state='sent' AND o.id NOT IN (SELECT id FROM retired_outgoing)")
            ] {
                let mut query = sql(journal.connection().prepare(statement))?;
                let rows = sql(query.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?))))?;
                for row in rows { let (id,at,size) = sql(row)?; if kind==0 || !protected_outputs.contains(&id) { candidates.push((kind,id,at as f64,size)); } }
            }
            for (key,value) in journal.kv_rows("SELECT key,value FROM kv WHERE key LIKE 'message:%'")? {
                if !protected_messages.iter().any(|p| key.starts_with(p)) && journal.sent(&string_ids(value.required("ids")?)?)? {
                    let size:i64=sql(journal.connection().query_row("SELECT length(CAST(value AS BLOB)) FROM kv WHERE key=?",[&key],|r|r.get(0)))?;
                    candidates.push((2,key,value.required("at")?.as_f64().ok_or_else(IvyError::internal)?,size));
                }
            }
            // Stable ties preserve the original inbox/outgoing/message scan order.
            candidates.sort_by(|a,b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
            for (kind,id,_,size) in candidates {
                if count <= limits.count && bytes <= limits.bytes { break; }
                match kind {
                    0 => {
                        let (sender,jid,message):(String,String,String)=sql(journal.connection().query_row("SELECT sender,jid,message FROM inbox WHERE id=?",[&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))))?;
                        journal.write("INSERT INTO retired_inbox VALUES(?,?,1) ON CONFLICT(id) DO UPDATE SET retired=1",vec![text(&id),text(identity(&[sender.into(),jid.into(),message.into()])?)])?;
                        journal.finish(&id)?;
                        for prefix in ["read-receipt:","local-input:","inbox-content:"] { journal.set(&format!("{prefix}{id}"),&IvyJson::Null)?; }
                        journal.write("UPDATE inbox SET message='',request=NULL,received=? WHERE id=?",vec![Value::Integer(9_007_199_254_740_991),text(id)])?;
                    }
                    1 => {
                        let (jid,body):(String,String)=sql(journal.connection().query_row("SELECT jid,text FROM outgoing WHERE id=?",[&id],|r|Ok((r.get(0)?,r.get(1)?))))?;
                        journal.write("INSERT OR IGNORE INTO retired_outgoing VALUES(?,?)",vec![text(&id),text(identity(&[jid.into(),body.into()])?)])?;
                        journal.write("UPDATE outgoing SET text='' WHERE id=?",vec![text(&id)])?;
                        for prefix in ["out-created:","send-attempt:","out-automatic:"] { journal.set(&format!("{prefix}{id}"),&IvyJson::Null)?; }
                    }
                    _ => journal.set(&id,&IvyJson::Null)?,
                }
                count-=1; bytes-=size;
            }
            Ok(())
        })
    }
}
fn string_ids(value: &IvyJson) -> IvyResult<Vec<String>> {
    value
        .as_array()
        .ok_or_else(IvyError::internal)?
        .iter()
        .map(|id| id.as_str())
        .collect()
}
