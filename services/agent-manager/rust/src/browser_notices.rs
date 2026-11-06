//! Native notice content and the service-owned completion/policy fences.
use crate::{
    inventory_manager::{InventoryConnection, InventoryManager},
    local_state::LocalAgentState,
    native_operations::NativeOperations,
    notice_policy::NativeNoticePolicy,
    service_link::ServiceLink,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json};
use ivy_host_runtime::codex_rpc::NativeRpc;
use ivy_sdk::{
    browser_notice::publish_browser_notice,
    rpc::{RequestOptions, RpcClient},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Semaphore;

const NOTICE_IN_FLIGHT: usize = 8;
const NOTICE_BYTES: usize = 1024 * 1024;

/// The Shared owner supplies URLSearchParams serialization, including its
/// USVString boundary. This owner only selects the original ordered fields.
pub type FormQuery = dyn Fn(&[(JsString, JsString)]) -> IvyResult<JsString> + Send + Sync;
fn target(node: &JsString, thread: &JsString, query: &FormQuery) -> IvyResult<IvyJson> {
    let mut fragment = JsString::from("#/task?");
    fragment
        .0
        .extend(query(&[("node".into(), node.clone()), ("id".into(), thread.clone())])?.0);
    Ok(IvyJson::object([
        ("uiId", "agent-ui".into()),
        ("fragment", IvyJson::String(fragment)),
    ]))
}
fn text<'a>(value: &'a IvyJson, key: &str) -> Option<&'a JsString> {
    value
        .get(key)
        .and_then(IvyJson::as_js_string)
        .filter(|v| !v.0.is_empty())
}
pub fn native_browser_notice(
    node: &JsString,
    event: &IvyJson,
    query: &FormQuery,
) -> IvyResult<Option<IvyJson>> {
    if event.get("method") != Some(&"turn/completed".into()) {
        return Ok(None);
    }
    let Some(params) = event.get("params") else {
        return Ok(None);
    };
    let Some(thread) = text(params, "threadId") else {
        return Ok(None);
    };
    let Some(turn) = params.get("turn") else {
        return Ok(None);
    };
    let Some(turn_id) = text(turn, "id") else {
        return Ok(None);
    };
    if turn.get("status") == Some(&"interrupted".into()) {
        return Ok(None);
    }
    Ok(Some(IvyJson::object([
        (
            "title",
            if turn.get("status") == Some(&"failed".into()) {
                "Ivy · Agent needs attention"
            } else {
                "Ivy · Agent finished"
            }
            .into(),
        ),
        ("body", "Open the conversation to review the result.".into()),
        (
            "tag",
            hash_json(&IvyJson::Array(vec![
                IvyJson::String(node.clone()),
                IvyJson::String(thread.clone()),
                IvyJson::String(turn_id.clone()),
            ]))?
            .into(),
        ),
        ("target", target(node, thread, query)?),
    ])))
}
pub fn input_browser_notice(input: &IvyJson, query: &FormQuery) -> IvyResult<Option<IvyJson>> {
    let Some(thread) = text(input, "threadId") else {
        return Ok(None);
    };
    if input.get("state") != Some(&"pending".into())
        || input.get("method") == Some(&"currentTime/read".into())
    {
        return Ok(None);
    }
    let identity = input.required("identity")?;
    let node = identity
        .required("serviceNodeId")?
        .as_js_string()
        .ok_or_else(IvyError::internal)?;
    Ok(Some(IvyJson::object([
        ("title", "Ivy · Input needed".into()),
        ("body", "An agent is waiting for your response.".into()),
        ("tag", hash_json(identity)?.into()),
        ("target", target(node, thread, query)?),
    ])))
}
pub async fn service_owns_completion(
    client: &dyn RpcClient,
    caller: Option<&JsString>,
) -> IvyResult<bool> {
    let Some(caller) = caller.filter(|v| !v.0.is_empty()) else {
        return Ok(false);
    };
    let mut cursor = None;
    loop {
        let mut params = IvyJson::object([("limit", IvyJson::Number(200.0))]);
        if let Some(cursor) = cursor {
            params.insert("cursor", IvyJson::String(cursor));
        }
        let page = client
            .request("serviceNodes.list", params, RequestOptions::default())
            .await?;
        if page
            .required("items")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .any(|node| {
                node.get("principalId") == Some(&IvyJson::String(caller.clone()))
                    && node.get("serviceName") != Some(&"agent-manager".into())
            })
        {
            return Ok(true);
        }
        cursor = text(&page, "nextCursor").cloned();
        if cursor.is_none() {
            return Ok(false);
        }
    }
}

pub struct BrowserNoticeSettings {
    pub service: ServiceLink,
    pub closing: Arc<AtomicBool>,
    pub operations: NativeOperations,
    pub state: Arc<Mutex<LocalAgentState>>,
    pub inventory: InventoryManager,
    pub policy: Arc<Mutex<NativeNoticePolicy>>,
    pub rpc: NativeRpc,
}
#[derive(Clone)]
pub struct BrowserNotices(Arc<Inner>);
struct Inner {
    settings: BrowserNoticeSettings,
    slots: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
}
pub(crate) struct Notice {
    pub thread: JsString,
    pub payload: IvyJson,
    pub turn: Option<JsString>,
}
impl BrowserNotices {
    pub fn new(settings: BrowserNoticeSettings) -> Self {
        Self(Arc::new(Inner {
            settings,
            slots: Arc::new(Semaphore::new(NOTICE_IN_FLIGHT)),
            bytes: Arc::new(Semaphore::new(NOTICE_BYTES)),
        }))
    }
    /// Optional browser delivery must not accumulate tasks behind slow Hive or
    /// metadata reads, or wait on the native actor invoking this callback.
    /// Reserve before rendering; false asks the caller to account for a skipped completion.
    pub(crate) fn send(
        &self,
        create: impl FnOnce() -> IvyResult<Option<Notice>>,
    ) -> IvyResult<bool> {
        let settings = &self.0.settings;
        if !settings.service.ready() || settings.closing.load(Ordering::Acquire) {
            return Ok(false);
        }
        let Ok(slot) = self.0.slots.clone().try_acquire_owned() else {
            return Ok(false);
        };
        let Some(notice) = create()? else {
            return Ok(false);
        };
        // Reserve owned identity strings and the SDK publisher's payload copies.
        // Native/Hive response bodies have separate budgets.
        let bytes = notice
            .payload
            .retained_bytes()
            .saturating_mul(3)
            .saturating_add(notice.thread.0.capacity().saturating_mul(2))
            .saturating_add(
                notice
                    .turn
                    .as_ref()
                    .map_or(0, |v| v.0.capacity().saturating_mul(2)),
            );
        let owner = self.clone();
        let work = async move {
            let _ = owner
                .publish(notice.thread, notice.payload, notice.turn)
                .await;
        };
        // Charge the actual future layout plus permit/task allocation overhead.
        let bytes = bytes
            .saturating_add(std::mem::size_of_val(&work))
            .saturating_add(512);
        let Ok(bytes) = u32::try_from(bytes) else {
            return Ok(false);
        };
        let Ok(bytes) = self.0.bytes.clone().try_acquire_many_owned(bytes) else {
            return Ok(false);
        };
        tokio::spawn(async move {
            let (_slot, _bytes) = (slot, bytes);
            work.await;
        });
        Ok(true)
    }
    async fn receipt(
        &self,
        thread: &JsString,
        turn: &JsString,
        release: bool,
    ) -> IvyResult<Option<JsString>> {
        let state = self.0.settings.state.clone();
        // Node's SQLite text binding converts strings to UTF-8 at this boundary.
        let thread = String::from_utf16_lossy(&thread.0);
        let turn = String::from_utf16_lossy(&turn.0);
        tokio::task::spawn_blocking(move || {
            let state = state.lock().map_err(|_| IvyError::internal())?;
            if release {
                state.release_completion(&thread, &turn)?;
                Ok(None)
            } else {
                Ok(state.completion_caller(&thread, &turn)?.map(JsString::from))
            }
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
    async fn publish(
        &self,
        thread: JsString,
        notice: IvyJson,
        turn: Option<JsString>,
    ) -> IvyResult<()> {
        let settings = &self.0.settings;
        if !settings.service.ready() || settings.closing.load(Ordering::Acquire) {
            return Ok(());
        }
        if let Some(turn) = turn.as_ref().filter(|v| !v.0.is_empty()) {
            settings.operations.settle_turn(&thread).await;
            let caller = self.receipt(&thread, turn, false).await?;
            if service_owns_completion(&settings.service.connection()?, caller.as_ref()).await? {
                self.receipt(&thread, turn, true).await?;
                return Ok(());
            }
        }
        settings
            .inventory
            .ensure_projects(InventoryConnection::from_connection(
                settings.service.connection()?,
            ))
            .await?;
        let unknown = settings
            .policy
            .lock()
            .map_err(|_| IvyError::internal())?
            .is_internal(&thread)?
            .is_none();
        if unknown {
            NativeNoticePolicy::ensure(&settings.policy, &settings.rpc, thread.clone()).await?;
        }
        let external = settings
            .policy
            .lock()
            .map_err(|_| IvyError::internal())?
            .is_internal(&thread)?
            == Some(false);
        if external && settings.service.ready() && !settings.closing.load(Ordering::Acquire) {
            publish_browser_notice(
                Arc::new(settings.service.connection()?),
                &"agent".into(),
                &notice,
            )
            .await?;
        }
        if let Some(turn) = turn.as_ref().filter(|v| !v.0.is_empty()) {
            self.receipt(&thread, turn, true).await?;
        }
        Ok(())
    }
}
