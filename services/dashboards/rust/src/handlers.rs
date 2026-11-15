//! Seven service handlers. Connection lifecycle owns the current store and
//! browser owner; a handler retains the generation it started with.
use crate::{image_template, store::DashboardStore, validation::Input};
use base64::Engine;
use ivy_contracts::{
    IvyError, IvyJson as J, IvyResult, JsString, operation::derive_operation_id_js,
};
use ivy_sdk::service::{InvocationContext, ServiceFuture, ToolHandler};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

pub use crate::validation::OutputOptions as Output;
pub type Render =
    Arc<dyn Fn(JsString, J, Output, CancellationToken) -> ServiceFuture<Vec<u8>> + Send + Sync>;

#[derive(Clone)]
pub struct Handlers {
    store: Arc<RwLock<Option<DashboardStore>>>,
    render: Render,
    images: Arc<Semaphore>,
    fenced: Arc<Mutex<Option<OwnedSemaphorePermit>>>,
    retired: Arc<AtomicBool>,
}
impl Handlers {
    pub fn new(render: Render) -> Self {
        Self {
            store: Arc::new(RwLock::new(None)),
            render,
            images: Arc::new(Semaphore::new(1)),
            fenced: Arc::new(Mutex::new(None)),
            retired: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn set_store(&self, store: Option<DashboardStore>) {
        *self.store.write().unwrap() = store;
    }
    pub async fn idle(&self) {
        let _permit = self.images.acquire().await;
    }
    pub fn confirm_retirement(&self) {
        let mut fence = self.fenced.lock().unwrap();
        self.retired.store(true, Ordering::Release);
        fence.take();
    }
    pub fn map(&self) -> HashMap<JsString, ToolHandler> {
        ["list", "read", "save", "delete", "data", "render", "image"]
            .into_iter()
            .map(|name| {
                let owner = self.clone();
                let handler: ToolHandler = Arc::new(move |args, context| {
                    let owner = owner.clone();
                    Box::pin(async move { owner.invoke(name, args, context).await })
                });
                (format!("dashboards.{name}").into(), handler)
            })
            .collect()
    }
    pub async fn invoke(&self, name: &str, args: J, context: InvocationContext) -> IvyResult<J> {
        let store = self
            .store
            .read()
            .unwrap()
            .clone()
            .ok_or_else(|| IvyError::new("service_not_ready", "Dashboards is connecting."))?;
        let args = Input::parse(name, &args)?;
        let operation = context.operation_id.filter(|s| !s.0.is_empty());
        if args.is_write() && operation.is_none() {
            return Err(IvyError::new(
                "invalid_arguments",
                "An operationId is required for writes.",
            ));
        }
        let operation = match operation {
            Some(parent) => derive_operation_id_js(
                &parent,
                &J::object([
                    ("service", "dashboards".into()),
                    ("caller", J::String(context.caller_principal_id)),
                ]),
            )?,
            None => JsString::default(),
        };
        let (id, token, output, head, condition) = match args {
            Input::List { cursor, limit } => return store.list(cursor, limit).await,
            Input::Save { value, target } => {
                let (id, revision) = match target {
                    Some((id, revision)) => (Some(id), Some(revision as f64)),
                    None => (None, None),
                };
                return store.save(value.into_json(), id, revision, operation).await;
            }
            Input::Delete {
                id,
                expected_revision,
            } => {
                return store.remove(id, expected_revision as f64, operation).await;
            }
            Input::Read { id } => return store.read(id).await,
            Input::Data { id, html_hash } => {
                let signal = context.signal.child_token();
                let _cancel_on_drop = signal.clone().drop_guard();
                let mut row = store.read(id).await?;
                let mut value = row.remove("value").ok_or_else(IvyError::internal)?;
                let data = store
                    .data_with_options(
                        &value,
                        ivy_sdk::rpc::RequestOptions {
                            cancellation: signal,
                            ..Default::default()
                        },
                    )
                    .await?;
                let mut hash = Sha256::new();
                for unit in &value.required("html")?.as_js_string().unwrap().0 {
                    hash.update(unit.to_le_bytes());
                }
                let hash = format!(
                    "sha256:{}",
                    hash.finalize()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                );
                if html_hash.as_ref() == Some(&hash.as_str().into()) {
                    value.remove("html");
                }
                value.remove("image");
                row.insert("htmlHash", hash.into());
                row.insert("value", value);
                return Ok(J::object([
                    ("data", data),
                    ("dashboard", row),
                    (
                        "observedAt",
                        chrono::Utc::now()
                            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                            .into(),
                    ),
                ]));
            }
            Input::Render { id, output } => (id, None, output, false, None),
            Input::Image {
                id,
                token,
                output,
                head,
                if_none_match,
            } => (id, Some(token), output, head, if_none_match),
        };
        let row = if let Some(token) = token {
            store.image(id, &token).await?
        } else {
            store.read(id).await?
        };
        // Token/workspace authorization precedes admission; expensive source
        // reads, native pixels and browser ownership all share this one slot.
        let permit = self.images.clone().try_acquire_owned().map_err(|_| {
            IvyError::new(
                "resource_busy",
                "An image is already rendering; retry shortly.",
            )
        })?;
        let signal = context.signal.child_token();
        let _cancel_on_drop = signal.clone().drop_guard();
        let render = self.render.clone();
        let (fenced, retired) = (self.fenced.clone(), self.retired.clone());
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut permit = Some(permit);
            let result = async {
                let mut row = row;
                let mut value = row.remove("value").ok_or_else(IvyError::internal)?;
                let data = tokio::select! {
                    biased;
                    _=signal.cancelled()=>return Err(IvyError::new("cancelled", "Image request cancelled.")),
                    result=store.data_with_options(&value, ivy_sdk::rpc::RequestOptions { cancellation: signal.clone(), ..Default::default() })=>result?,
                };
                let mut metadata = value.remove("metadata").ok_or_else(IvyError::internal)?;
                metadata.insert("refreshSeconds", value.required("refreshSeconds")?.clone());
                let mut response = J::object([
            ("mimeType", "image/png".into()),
            ("encoding", "base64".into()),
            ("width", J::Number(f64::from(output.width))),
            ("height", J::Number(f64::from(output.height))),
            ("colorMode", output.color_mode().into()),
            ("metadata", metadata),
                ]);
                let png = if let Some(image) = value.get("image") {
                    let template = image.required("svg")?.as_str()?;
                    let prepared = tokio::task::spawn_blocking(move || image_template::prepare(&template, data, output, chrono::Utc::now())).await.map_err(|_| IvyError::internal())??;
                    response.insert("etag", prepared.etag.as_str().into());
                    if condition.as_ref().is_some_and(|c| image_template::matches(&String::from_utf16_lossy(&c.0), &prepared.etag)) {
                        response.insert("notModified", true.into()); return Ok(response);
                    }
                    if head { response.insert("head", true.into()); return Ok(response); }
                    if signal.is_cancelled() { return Err(IvyError::new("cancelled", "Image request cancelled.")); }
                    // Once started, native work is observed to completion even if
                    // the client disappears. It cannot outlive render admission.
                    tokio::task::spawn_blocking(move || prepared.render()).await.map_err(|_| IvyError::internal())??
                } else {
                    let mut html = value.remove("html").ok_or_else(IvyError::internal)?;
                    let J::String(html) = &mut html else { return Err(IvyError::internal()); };
                    (render)(std::mem::take(html), data, output, signal.clone()).await?
                };
                if signal.is_cancelled() { return Err(IvyError::new("cancelled", "Image request cancelled.")); }
                response.insert("data", base64::engine::general_purpose::STANDARD.encode(png).into());
                Ok(response)
            }.await;
            if result.as_ref().is_err_and(|e| e.code == "outcome_unknown") {
                // Unknown process retirement must not admit further source
                // work. Shutdown releases the fence only after confirming exit.
                let mut fence = fenced.lock().unwrap();
                if !retired.load(Ordering::Acquire) {
                    *fence = permit.take();
                }
            }
            drop(permit);
            let _ = tx.send(result);
        });
        rx.await.map_err(|_| IvyError::internal())?
    }
}
