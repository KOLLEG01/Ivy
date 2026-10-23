//! Phone's configured control-connection policy. The supplied connector retains
//! the shared process owner; this module never launches or kills a second child.
use super::{
    VoiceModelCatalog, VoiceModelControl, VoiceRpc, VoiceSchemas, VoiceSpeakerCatalog,
    VoiceSpeakerControl, VoiceSpeakerSettings,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json};
use ivy_host_runtime::{
    codex_rpc::Events,
    node_path::{self, Context},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::watch;

pub type VoiceClose = Pin<Box<dyn Future<Output = IvyResult<()>> + Send>>;
pub type VoiceConnect = Pin<Box<dyn Future<Output = IvyResult<VoiceConnection>> + Send>>;

/// The original close delegate must retain the shared native process owner and
/// join its memoized stop result, including uncertain cleanup. No retry is added.
#[derive(Clone)]
pub struct VoiceConnection {
    pub rpc: ivy_host_runtime::codex_rpc::NativeRpc,
    pub home: JsString,
    pub close: Arc<dyn Fn() -> VoiceClose + Send + Sync>,
}
pub trait VoiceConnector: Send + Sync {
    fn connect(&self, options: VoiceConnectOptions, events: Events) -> VoiceConnect;
}

/// Already admitted service settings. Keep complete native settings/credentials
/// opaque; path resolution uses the shared executing-account context snapshot.
pub struct VoiceControlSettings {
    pub native: IvyJson,
    pub artifact_root: JsString,
    pub data_root: JsString,
    pub credential: Option<JsString>,
    pub paths: Context,
}
#[derive(Clone)]
pub struct VoiceConnectOptions {
    pub settings: IvyJson,
    pub artifact_root: JsString,
    pub data_root: JsString,
    pub credential: Option<JsString>,
    pub client_name: &'static str,
    pub client_version: &'static str,
}
/// The enclosing Voice task owner supplies its existing request/notification/loss
/// handlers and clears reconciled/prepared state at each fresh connection attempt.
pub struct VoiceControlHooks {
    pub events: Events,
    pub on_open: Arc<dyn Fn() + Send + Sync>,
    pub on_error: Arc<dyn Fn(&str) + Send + Sync>,
}

#[derive(Clone)]
pub struct VoiceControlOperation<T: Clone>(watch::Receiver<Option<IvyResult<T>>>);
impl<T: Clone> VoiceControlOperation<T> {
    pub(super) fn from_receiver(receiver: watch::Receiver<Option<IvyResult<T>>>) -> Self {
        Self(receiver)
    }
    pub async fn wait(&self) -> IvyResult<T> {
        let mut result = self.0.clone();
        loop {
            if let Some(value) = result.borrow_and_update().clone() {
                return value;
            }
            result.changed().await.map_err(|_| IvyError::internal())?;
        }
    }
    fn ready(value: IvyResult<T>) -> Self {
        let (_, result) = watch::channel(Some(value));
        Self(result)
    }
}
struct Connected {
    voice: VoiceRpc,
    native: VoiceConnection,
}
#[derive(Default)]
struct State {
    connection: Option<Arc<Connected>>,
    opening: Option<VoiceControlOperation<VoiceRpc>>,
    closing: Option<VoiceControlOperation<()>>,
    closed: bool,
    fingerprint: Option<String>,
}
struct Inner {
    options: VoiceConnectOptions,
    paths: Context,
    connector: Arc<dyn VoiceConnector>,
    schemas: Arc<dyn VoiceSchemas>,
    hooks: VoiceControlHooks,
    state: Mutex<State>,
}
/// Cloned catalogs share one control connection and their already qualified
/// per-connection caches. Their required compiler boundary stays explicit.
#[derive(Clone)]
pub struct VoiceControl {
    owner: Arc<Inner>,
    models: VoiceModelCatalog,
    speakers: VoiceSpeakerCatalog,
}
struct CatalogControl(Arc<Inner>);
impl VoiceModelControl for CatalogControl {
    fn open(&self) -> Pin<Box<dyn Future<Output = IvyResult<VoiceRpc>> + Send + '_>> {
        let work = self.0.open(false);
        Box::pin(async move { work.wait().await })
    }
    fn current(&self) -> Option<VoiceRpc> {
        self.0.current()
    }
}
impl VoiceSpeakerControl for CatalogControl {
    fn speaker_settings(&self) -> VoiceSpeakerSettings {
        VoiceSpeakerSettings {
            cwd: self
                .0
                .options
                .settings
                .get("cwd")
                .and_then(IvyJson::as_js_string)
                .unwrap()
                .clone(),
            voice: self
                .0
                .options
                .settings
                .get("voice")
                .and_then(IvyJson::as_js_string)
                .cloned(),
        }
    }
}
fn stopping() -> IvyError {
    IvyError::new("service_not_ready", "Codex Voice is stopping.")
}
impl VoiceControl {
    pub(super) fn native_settings(&self) -> &IvyJson {
        &self.owner.options.settings
    }
    pub(super) fn paths(&self) -> &Context {
        &self.owner.paths
    }
    pub fn new(
        settings: VoiceControlSettings,
        connector: Arc<dyn VoiceConnector>,
        schemas: Arc<dyn VoiceSchemas>,
        hooks: VoiceControlHooks,
    ) -> IvyResult<Self> {
        let cwd = settings.native.get("cwd").and_then(IvyJson::as_js_string);
        if cwd.is_none_or(|cwd| !node_path::is_absolute(settings.paths.style, cwd)) {
            return Err(IvyError::new(
                "invalid_arguments",
                "Codex Voice requires its explicit local working directory.",
            ));
        }
        let owner = Arc::new(Inner {
            options: VoiceConnectOptions {
                settings: settings.native,
                artifact_root: settings.artifact_root,
                data_root: node_path::join(
                    settings.paths.style,
                    &[settings.data_root, "codex-runtime".into()],
                ),
                credential: settings.credential.filter(|s| !s.0.is_empty()),
                client_name: "ivy-phone-bridge",
                client_version: "0.1.0",
            },
            paths: settings.paths,
            connector,
            schemas,
            hooks,
            state: Mutex::new(State::default()),
        });
        let control = Arc::new(CatalogControl(owner.clone()));
        Ok(Self {
            owner,
            models: VoiceModelCatalog::new(control.clone()),
            speakers: VoiceSpeakerCatalog::new(control),
        })
    }
    /// Admission and owned work start before returning the observer. Losing any
    /// caller cannot abandon a configured process or strand the opening latch.
    pub fn open(&self) -> VoiceControlOperation<VoiceRpc> {
        self.owner.open(false)
    }
    /// Original session stop may reconnect to its same configured daemon/home
    /// after ordinary admission closes. The final close joins that connection.
    pub fn open_cleanup(&self) -> VoiceControlOperation<VoiceRpc> {
        self.owner.open(true)
    }
    pub fn current(&self) -> Option<VoiceRpc> {
        self.owner.current()
    }
    pub fn fingerprint(&self) -> Option<String> {
        self.owner.state.lock().unwrap().fingerprint.clone()
    }
    pub fn is_stopping(&self) -> bool {
        self.owner.state.lock().unwrap().closed
    }
    pub async fn models(&self) -> IvyResult<IvyJson> {
        self.models.models().await
    }
    pub async fn speaker_voices(&self) -> IvyResult<IvyJson> {
        self.speakers.speaker_voices().await
    }

    /// The enclosing owner supplies its original preparation/archive/session
    /// drain. It runs after an in-flight open, before the final native close.
    /// Cleanup errors override drain errors, preserving the original finally.
    pub fn close(&self, drain: VoiceClose) -> VoiceControlOperation<()> {
        let (completion, receive) = watch::channel(None);
        let operation = VoiceControlOperation(receive);
        let opening = {
            let mut state = self.owner.state.lock().unwrap();
            if let Some(closing) = &state.closing {
                return closing.clone();
            }
            state.closed = true;
            state.closing = Some(operation.clone());
            state.opening.clone()
        };
        let owner = self.owner.clone();
        tokio::spawn(async move {
            let result = tokio::spawn(async move {
                if let Some(opening) = opening {
                    let _ = opening.wait().await;
                }
                let result = drain.await;
                let current = owner.state.lock().unwrap().connection.clone();
                if let Some(current) = current {
                    (current.native.close)().await?;
                }
                result
            })
            .await
            .unwrap_or_else(|_| Err(IvyError::internal()));
            completion.send_replace(Some(result));
        });
        operation
    }
}
impl Inner {
    fn current(&self) -> Option<VoiceRpc> {
        self.state
            .lock()
            .unwrap()
            .connection
            .as_ref()
            .map(|c| c.voice.clone())
    }
    fn events(self: &Arc<Self>) -> Events {
        let weak: Weak<Self> = Arc::downgrade(self);
        let events = self.hooks.events.clone();
        Events {
            on_request: events.on_request,
            on_notification: events.on_notification,
            on_close: Arc::new(move |code| {
                let weak = weak.clone();
                let handler = events.on_close.clone();
                Box::pin(async move {
                    handler(code.clone()).await?;
                    if let Some(owner) = weak.upgrade() {
                        let closed = owner.state.lock().unwrap().closed;
                        if !closed {
                            (owner.hooks.on_error)(&code);
                        }
                    }
                    Ok(())
                })
            }),
        }
    }
    fn open(self: &Arc<Self>, cleanup: bool) -> VoiceControlOperation<VoiceRpc> {
        let (completion, receive) = watch::channel(None);
        let operation = VoiceControlOperation(receive);
        {
            let mut state = self.state.lock().unwrap();
            if state.closed && !cleanup {
                return VoiceControlOperation::ready(Err(stopping()));
            }
            if let Some(connection) = state
                .connection
                .as_ref()
                .filter(|c| c.voice.transport().connected())
            {
                return VoiceControlOperation::ready(Ok(connection.voice.clone()));
            }
            if let Some(opening) = &state.opening {
                return opening.clone();
            }
            state.opening = Some(operation.clone());
        }
        let owner = self.clone();
        tokio::spawn(async move {
            let work = owner.clone();
            let result = tokio::spawn(async move {
                (work.hooks.on_open)();
                let native = work
                    .connector
                    .connect(work.options.clone(), work.events())
                    .await?;
                let connection = Arc::new(Connected {
                    voice: VoiceRpc::from_native(native.rpc.clone(), work.schemas.clone()),
                    native,
                });
                // Publish before project/read, as in the original open prefix.
                // Even a failed fingerprint/project read retains cleanup authority.
                work.state.lock().unwrap().connection = Some(connection.clone());
                let settings = &work.options.settings;
                let fingerprint = hash_json(&IvyJson::object([
                    ("home", IvyJson::String(connection.native.home.clone())),
                    (
                        "cwd",
                        IvyJson::String(
                            work.paths.resolve(&[settings
                                .get("cwd")
                                .and_then(IvyJson::as_js_string)
                                .unwrap()
                                .clone()]),
                        ),
                    ),
                    (
                        "projectId",
                        settings.get("projectId").cloned().unwrap_or(IvyJson::Null),
                    ),
                    (
                        "config",
                        settings
                            .get("config")
                            .filter(|v| !v.is_null())
                            .cloned()
                            .unwrap_or_else(|| IvyJson::object([] as [(&str, IvyJson); 0])),
                    ),
                ]))?;
                let closed = {
                    let mut state = work.state.lock().unwrap();
                    state.fingerprint = Some(fingerprint);
                    state.closed
                };
                if closed && !cleanup {
                    (connection.native.close)().await?;
                    return Err(IvyError::new(
                        "service_not_ready",
                        "Codex Voice stopped during initialization.",
                    ));
                }
                if let Some(project) = settings
                    .get("projectId")
                    .and_then(IvyJson::as_js_string)
                    .filter(|s| !s.0.is_empty())
                {
                    let value = connection
                        .voice
                        .call(
                            "project/read",
                            IvyJson::object([("projectId", IvyJson::String(project.clone()))]),
                            Default::default(),
                            None,
                        )
                        .await?;
                    if value.get("project").and_then(|p| p.get("id"))
                        != Some(&IvyJson::String(project.clone()))
                    {
                        return Err(IvyError::new(
                            "phone_voice_project_missing",
                            "Configured Voice project is unavailable in this Codex home.",
                        ));
                    }
                }
                Ok(connection.voice.clone())
            })
            .await
            .unwrap_or_else(|_| Err(IvyError::internal()));
            owner.state.lock().unwrap().opening = None;
            completion.send_replace(Some(result));
        });
        operation
    }
}

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_voice_control.rs"]
pub(crate) mod tests;
