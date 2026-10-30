//! Durable ownership of run-local native MCP contexts, separate from task results.
use crate::{model, store_owner::StoreOwner};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    json::{parse, stringify},
};
use ivy_sdk::{native_evidence::NativeOwnerIdentity, native_operation::NativeOperationCall};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const PREFIX: &str = "native-mcp:";
const MAX_CONTEXTS: usize = 64;

pub(crate) fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn call_value(call: &NativeOperationCall) -> IvyJson {
    IvyJson::object([
        ("operationId", IvyJson::String(call.operation_id.clone())),
        ("method", IvyJson::String(call.method.clone())),
        ("params", call.params.clone()),
        (
            "definitionHash",
            IvyJson::String(call.definition_hash.clone()),
        ),
    ])
}
fn call(value: &IvyJson) -> IvyResult<NativeOperationCall> {
    Ok(NativeOperationCall {
        operation_id: text(value, "operationId")?,
        method: text(value, "method")?,
        params: value.required("params")?.clone(),
        definition_hash: text(value, "definitionHash")?,
    })
}
pub(crate) struct Context {
    pub revision: i64,
    pub session_id: String,
    pub caller: JsString,
    pub owner: NativeOwnerIdentity,
    pub epoch: JsString,
    pub start: NativeOperationCall,
    pub thread: Option<JsString>,
    pub pending: Option<NativeOperationCall>,
    pub unsubscribe: Option<NativeOperationCall>,
    pub closing: bool,
}
impl Context {
    pub fn key(&self) -> IvyResult<String> {
        Ok(format!("{PREFIX}{}", self.start.operation_id.to_utf8()?))
    }
    fn encode(&self, revision: i64) -> IvyResult<String> {
        stringify(
            &IvyJson::object([
                ("revision", IvyJson::Number(revision as f64)),
                ("sessionId", self.session_id.clone().into()),
                ("caller", IvyJson::String(self.caller.clone())),
                (
                    "owner",
                    IvyJson::object([
                        (
                            "serviceNodeId",
                            IvyJson::String(self.owner.service_node_id.clone()),
                        ),
                        (
                            "hostId",
                            self.owner
                                .host_id
                                .clone()
                                .map(IvyJson::String)
                                .unwrap_or(IvyJson::Null),
                        ),
                        (
                            "nativeVersion",
                            IvyJson::String(self.owner.native_version.clone()),
                        ),
                        (
                            "nativeExecutableHash",
                            IvyJson::String(self.owner.native_executable_hash.clone()),
                        ),
                        (
                            "catalogHash",
                            IvyJson::String(self.owner.catalog_hash.clone()),
                        ),
                    ]),
                ),
                ("epoch", IvyJson::String(self.epoch.clone())),
                ("start", call_value(&self.start)),
                (
                    "thread",
                    self.thread
                        .clone()
                        .map(IvyJson::String)
                        .unwrap_or(IvyJson::Null),
                ),
                (
                    "pending",
                    self.pending
                        .as_ref()
                        .map(call_value)
                        .unwrap_or(IvyJson::Null),
                ),
                (
                    "unsubscribe",
                    self.unsubscribe
                        .as_ref()
                        .map(call_value)
                        .unwrap_or(IvyJson::Null),
                ),
                ("closing", self.closing.into()),
            ]),
            usize::MAX,
        )
    }
    fn decode(source: &str) -> IvyResult<Self> {
        let value = parse(source, usize::MAX)?;
        let owner = value.required("owner")?;
        Ok(Self {
            revision: model::number(&value, "revision")? as i64,
            session_id: model::text(&value, "sessionId")?,
            caller: text(&value, "caller")?,
            owner: NativeOwnerIdentity {
                service_node_id: text(owner, "serviceNodeId")?,
                host_id: owner
                    .get("hostId")
                    .filter(|v| !v.is_null())
                    .map(|_| text(owner, "hostId"))
                    .transpose()?,
                native_version: text(owner, "nativeVersion")?,
                native_executable_hash: text(owner, "nativeExecutableHash")?,
                catalog_hash: text(owner, "catalogHash")?,
            },
            epoch: text(&value, "epoch")?,
            start: call(value.required("start")?)?,
            thread: value
                .get("thread")
                .filter(|v| !v.is_null())
                .map(|_| text(&value, "thread"))
                .transpose()?,
            pending: value
                .get("pending")
                .filter(|v| !v.is_null())
                .map(call)
                .transpose()?,
            unsubscribe: value
                .get("unsubscribe")
                .filter(|v| !v.is_null())
                .map(call)
                .transpose()?,
            closing: value
                .required("closing")?
                .as_bool()
                .ok_or_else(IvyError::internal)?,
        })
    }
}
#[derive(Clone)]
pub(crate) struct Contexts {
    pub store: StoreOwner,
    pub session_id: String,
    // Cleanup and its durable transitions have one owner, including timed-out closes.
    pub cleanup: Arc<Mutex<String>>,
}
impl Contexts {
    pub fn new(store: StoreOwner) -> Self {
        Self {
            store,
            session_id: uuid::Uuid::new_v4().to_string(),
            cleanup: Arc::new(Mutex::new(String::new())),
        }
    }
    pub async fn insert(&self, context: &Context, closed: &CancellationToken) -> IvyResult<()> {
        let key = context.key()?;
        let value = context.encode(context.revision)?;
        let closed = closed.clone();
        self.store
            .call(move |store| {
                store.transaction(|store| {
                    if closed.is_cancelled() {
                        return Err(IvyError::internal());
                    }
                    let count: i64 = store
                        .connection()
                        .query_row(
                            "SELECT COUNT(*) FROM metadata WHERE key GLOB 'native-mcp:*'",
                            [],
                            |r| r.get(0),
                        )
                        .map_err(|_| IvyError::internal())?;
                    if count >= MAX_CONTEXTS as i64 {
                        return Err(IvyError::new(
                            "provider_unavailable",
                            "Outstanding MCP contexts must be retired before another can start.",
                        ));
                    }
                    store
                        .connection()
                        .execute("INSERT INTO metadata VALUES (?,?)", [&key, &value])
                        .map_err(|_| IvyError::internal())?;
                    Ok(())
                })
            })
            .await
    }
    pub async fn save(&self, context: &mut Context) -> IvyResult<()> {
        let key = context.key()?;
        let expected = context.revision;
        let revision = expected.checked_add(1).ok_or_else(IvyError::internal)?;
        let value = context.encode(revision)?;
        self.store
            .call(move |store| {
                use ivy_store::rusqlite::params;
                let changed = store
                    .connection()
                    .execute("UPDATE metadata SET value=? WHERE key=? AND json_extract(value,'$.revision')=?", params![value,key,expected])
                    .map_err(|_| IvyError::internal())?;
                if changed == 1 {
                    Ok(())
                } else {
                    Err(IvyError::internal())
                }
            })
            .await?;
        context.revision = revision;
        Ok(())
    }
    pub async fn mark_closing(&self, context: &Context) -> IvyResult<()> {
        let key = context.key()?;
        self.store.call(move |store| {
            // In-flight writes cannot overwrite this transition with an older snapshot.
            store.connection().execute("UPDATE metadata SET value=json_set(value,'$.closing',json('true'),'$.revision',json_extract(value,'$.revision')+1) WHERE key=?", [key]).map_err(|_| IvyError::internal())?;
            Ok(())
        }).await
    }
    pub async fn read(&self, key: String) -> IvyResult<Option<Context>> {
        self.store
            .call(move |store| {
                let context = store
                    .metadata(&key)?
                    .map(|value| Context::decode(&value))
                    .transpose()?;
                if context
                    .as_ref()
                    .is_some_and(|c| c.key().ok().as_ref() != Some(&key))
                {
                    return Err(IvyError::internal());
                }
                Ok(context)
            })
            .await
    }
    pub async fn next(&self, after: String) -> IvyResult<Option<String>> {
        let session = self.session_id.clone();
        self.store.call(move |store| {
            use ivy_store::rusqlite::{OptionalExtension, params};
            store.connection().query_row(
                "SELECT key FROM metadata WHERE key GLOB 'native-mcp:*' AND (json_extract(value,'$.closing')=1 OR json_extract(value,'$.sessionId')<>?) ORDER BY key<=?,key LIMIT 1",
                params![session,after], |r| r.get(0)
            ).optional().map_err(|_| IvyError::internal())
        }).await
    }
    pub async fn retire(&self, context: &Context) -> IvyResult<()> {
        let key = context.key()?;
        self.store
            .call(move |store| {
                store
                    .connection()
                    .execute("DELETE FROM metadata WHERE key=?", [key])
                    .map_err(|_| IvyError::internal())?;
                Ok(())
            })
            .await
    }
}
