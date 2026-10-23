//! Seven service handlers. Connection lifecycle owns the current store and
//! browser owner; a handler retains the generation it started with.
use crate::{store::DashboardStore, validation::Input};
use base64::Engine;
use ivy_contracts::{
    IvyError, IvyJson as J, IvyResult, JsString, operation::derive_operation_id_js,
};
use ivy_sdk::service::{InvocationContext, ServiceFuture, ToolHandler};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};
use tokio_util::sync::CancellationToken;

pub use crate::validation::OutputOptions as Output;
pub type Render =
    Arc<dyn Fn(JsString, J, Output, CancellationToken) -> ServiceFuture<Vec<u8>> + Send + Sync>;

#[derive(Clone)]
pub struct Handlers {
    store: Arc<RwLock<Option<DashboardStore>>>,
    render: Render,
}
impl Handlers {
    pub fn new(render: Render) -> Self {
        Self {
            store: Arc::new(RwLock::new(None)),
            render,
        }
    }
    pub fn set_store(&self, store: Option<DashboardStore>) {
        *self.store.write().unwrap() = store;
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
        let (id, token, output) = match args {
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
            Input::Data { id } => {
                let row = store.read(id).await?;
                let data = store.data(row.required("value")?).await?;
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
            Input::Render { id, output } => (id, None, output),
            Input::Image { id, token, output } => (id, Some(token), output),
        };
        let row = if let Some(token) = token {
            store.image(id, &token).await?
        } else {
            store.read(id).await?
        };
        let value = row.required("value")?;
        let data = store.data(value).await?;
        let png = (self.render)(
            value.required("html")?.as_js_string().unwrap().clone(),
            data,
            output,
            context.signal,
        )
        .await?;
        let mut metadata = value.required("metadata")?.clone();
        metadata.insert("refreshSeconds", value.required("refreshSeconds")?.clone());
        Ok(J::object([
            ("mimeType", "image/png".into()),
            ("encoding", "base64".into()),
            (
                "data",
                base64::engine::general_purpose::STANDARD.encode(png).into(),
            ),
            ("width", J::Number(f64::from(output.width))),
            ("height", J::Number(f64::from(output.height))),
            ("colorMode", output.color_mode().into()),
            ("metadata", metadata),
        ]))
    }
}

#[cfg(feature = "browser")]
pub fn native_renderer(renderer: Arc<crate::cdp::CdpRenderer>) -> Render {
    Arc::new(move |html, data, output, cancel| {
        let renderer = renderer.clone();
        Box::pin(async move {
            let input = crate::cdp::RenderInput::new(
                &html,
                &data,
                output.width,
                output.height,
                output.mode,
            )?;
            renderer
                .render(input, &cancel)
                .await
                .map_err(crate::cdp::RenderError::into_ivy)
        })
    })
}
