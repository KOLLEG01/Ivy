//! Original V3 speaker discovery over the connection owner's advertised V1
//! catalog. This does not start a Voice session or change a retained selection.
use super::{VoiceModelControl, models::wall_time_ms};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::codex_rpc::RequestOptions;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::watch;

#[derive(Clone, Debug)]
pub struct VoiceSpeakerSettings {
    pub cwd: JsString,
    pub voice: Option<JsString>,
}

/// The same Voice connection owner supplies its current configured settings.
pub trait VoiceSpeakerControl: VoiceModelControl {
    fn speaker_settings(&self) -> VoiceSpeakerSettings;
}

type Loading = watch::Receiver<Option<IvyResult<IvyJson>>>;
struct Cache {
    connection: Weak<IvyJson>,
    catalog: IvyJson,
    expires_at: i128,
}
#[derive(Default)]
struct State {
    cache: Option<Cache>,
    loading: Option<Loading>,
}
struct Inner {
    control: Arc<dyn VoiceSpeakerControl>,
    now: Arc<dyn Fn() -> i128 + Send + Sync>,
    state: Mutex<State>,
}

#[derive(Clone)]
pub struct VoiceSpeakerCatalog(Arc<Inner>);

impl VoiceSpeakerCatalog {
    pub fn new(control: Arc<dyn VoiceSpeakerControl>) -> Self {
        Self::with_clock(control, Arc::new(wall_time_ms))
    }

    fn with_clock(
        control: Arc<dyn VoiceSpeakerControl>,
        now: Arc<dyn Fn() -> i128 + Send + Sync>,
    ) -> Self {
        Self(Arc::new(Inner {
            control,
            now,
            state: Mutex::new(State::default()),
        }))
    }

    pub async fn speaker_voices(&self) -> IvyResult<IvyJson> {
        let connection = self.0.control.open().await?;
        let current = self
            .0
            .control
            .current()
            .is_some_and(|rpc| rpc.same_connection(&connection))
            && connection.transport().connected();
        let mut loading = {
            let mut state = self.0.state.lock().unwrap();
            if state.cache.as_ref().is_some_and(|cache| {
                !current
                    || !cache
                        .connection
                        .ptr_eq(&Arc::downgrade(&connection.catalog))
            }) {
                state.cache = None;
            }
            if let Some(cache) = &state.cache {
                if cache.expires_at > (self.0.now)() {
                    return Ok(cache.catalog.clone());
                }
            }
            if state
                .loading
                .as_ref()
                .is_some_and(|load| load.has_changed().is_err())
            {
                state.loading = None;
            }
            if let Some(loading) = &state.loading {
                loading.clone()
            } else {
                let (send, receive) = watch::channel(None);
                state.loading = Some(receive.clone());
                let owner = self.0.clone();
                tokio::spawn(async move {
                    let result = discover(&owner).await;
                    // Unlike model discovery, the original speaker operation has
                    // no initial-connection completion fence. A load spanning a
                    // reconnect is cached on the connection current at completion.
                    let identity = owner
                        .control
                        .current()
                        .map(|rpc| Arc::downgrade(&rpc.catalog))
                        .unwrap_or_default();
                    {
                        let mut state = owner.state.lock().unwrap();
                        if let Ok(catalog) = &result {
                            state.cache = Some(Cache {
                                connection: identity,
                                catalog: catalog.clone(),
                                expires_at: (owner.now)() + 60_000,
                            });
                        }
                        state.loading = None;
                    }
                    send.send_replace(Some(result));
                });
                receive
            }
        };
        drop(connection);
        loop {
            if let Some(result) = loading.borrow_and_update().as_ref() {
                return result.clone();
            }
            loading.changed().await.map_err(|_| IvyError::internal())?;
        }
    }
}

async fn call(owner: &Inner, method: &str, params: IvyJson) -> IvyResult<IvyJson> {
    let rpc = owner.control.current().ok_or_else(|| {
        IvyError::new(
            "phone_voice_unavailable",
            "Codex Voice transport is unavailable.",
        )
    })?;
    rpc.call(
        method,
        params,
        RequestOptions {
            detach_on_timeout: true,
            ..RequestOptions::default()
        },
        Some(15_000),
    )
    .await
}

async fn discover(owner: &Inner) -> IvyResult<IvyJson> {
    let result = call(
        owner,
        "thread/realtime/listVoices",
        IvyJson::object([] as [(&str, IvyJson); 0]),
    )
    .await?;
    // The original selects the CURRENT advertised definition after the reply.
    let rpc = owner.control.current().ok_or_else(IvyError::internal)?;
    let definition = rpc
        .catalog
        .get("clientRequests")
        .and_then(IvyJson::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("method") == Some(&"thread/realtime/listVoices".into()))
        })
        .ok_or_else(IvyError::internal)?;
    rpc.schemas.validate(
        definition
            .get("outputSchema")
            .ok_or_else(IvyError::internal)?,
        &result,
        64 * 1024,
    )?;
    drop(rpc);
    let voices = result.get("voices").ok_or_else(IvyError::internal)?;
    let ids = voices
        .get("v1")
        .and_then(IvyJson::as_array)
        .ok_or_else(IvyError::internal)?
        .iter()
        .map(|id| id.as_js_string().cloned().ok_or_else(IvyError::internal))
        .collect::<IvyResult<Vec<_>>>()?;
    let default = voices.get("defaultV1").and_then(IvyJson::as_js_string);
    if ids.is_empty()
        || ids.iter().collect::<HashSet<_>>().len() != ids.len()
        || !ids.iter().any(|id| Some(id) == default)
    {
        return Err(IvyError::new(
            "phone_speaker_catalog_invalid",
            "Native V3 speaker catalog is incomplete.",
        ));
    }
    let configuration = call(
        owner,
        "config/read",
        IvyJson::object([
            ("cwd", IvyJson::String(owner.control.speaker_settings().cwd)),
            ("includeLayers", false.into()),
        ]),
    )
    .await?;
    let config = configuration
        .get("config")
        .filter(|value| !value.is_null())
        .ok_or_else(IvyError::internal)?;
    let settings = owner.control.speaker_settings();
    let configured = config
        .get("realtime")
        .and_then(|value| value.get("voice"))
        .filter(|value| !value.is_null());
    let selected = settings.voice.as_ref().or_else(|| match configured {
        Some(value) => value.as_js_string(),
        None => default,
    });
    let selected = selected
        .filter(|selected| ids.contains(selected))
        .ok_or_else(|| {
            IvyError::new(
                "phone_speaker_voice_invalid",
                "Configured speaker voice is not supported by native V3.",
            )
        })?;
    Ok(IvyJson::object([
        ("protocol", "v3".into()),
        ("defaultVoiceId", IvyJson::String(selected.clone())),
        (
            "voices",
            IvyJson::Array(
                ids.iter()
                    .map(|id| {
                        Ok(IvyJson::object([
                            ("id", IvyJson::String(id.clone())),
                            ("displayName", IvyJson::String(display_name(id)?)),
                        ]))
                    })
                    .collect::<IvyResult<Vec<_>>>()?,
            ),
        ),
    ]))
}

fn display_name(id: &JsString) -> IvyResult<JsString> {
    let text = id.to_utf8()?;
    let mut chars = text.chars();
    Ok(match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect::<String>().into(),
        None => JsString::default(),
    })
}

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_voice_speakers.rs"]
mod tests;
