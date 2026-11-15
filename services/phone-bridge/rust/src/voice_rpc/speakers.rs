//! Select the highest advertised catalog supported by the pinned start contract.
use super::{VoiceModelControl, models::wall_time_ms};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::codex_rpc::{NativeRpc, RequestOptions};
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

#[derive(Debug)]
pub struct SpeakerVoice {
    pub id: JsString,
    display_name: JsString,
}

#[derive(Debug)]
pub struct VoiceSpeakers {
    pub protocol: JsString,
    pub catalog_version: JsString,
    pub default_voice_id: JsString,
    pub voices: Box<[SpeakerVoice]>,
}

impl VoiceSpeakers {
    pub fn contains(&self, id: &JsString) -> bool {
        self.voices.iter().any(|voice| &voice.id == id)
    }

    pub fn to_wire(&self) -> IvyJson {
        IvyJson::object([
            ("protocol", self.protocol.clone().into()),
            ("catalogVersion", self.catalog_version.clone().into()),
            ("defaultVoiceId", self.default_voice_id.clone().into()),
            (
                "voices",
                IvyJson::Array(
                    self.voices
                        .iter()
                        .map(|voice| {
                            IvyJson::object([
                                ("id", voice.id.clone().into()),
                                ("displayName", voice.display_name.clone().into()),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
    }

    pub fn from_wire(value: &IvyJson) -> IvyResult<Self> {
        let string = |v: &IvyJson, key: &str| {
            v.required(key)?
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)
        };
        Ok(Self {
            protocol: string(value, "protocol")?,
            catalog_version: string(value, "catalogVersion")?,
            default_voice_id: string(value, "defaultVoiceId")?,
            voices: value
                .required("voices")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .map(|voice| {
                    Ok(SpeakerVoice {
                        id: string(voice, "id")?,
                        display_name: string(voice, "displayName")?,
                    })
                })
                .collect::<IvyResult<_>>()?,
        })
    }
}

type Loading = watch::Receiver<Option<IvyResult<Arc<VoiceSpeakers>>>>;
struct Cache {
    connection: Weak<NativeRpc>,
    catalog: Arc<VoiceSpeakers>,
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

    pub async fn speaker_voices(&self) -> IvyResult<Arc<VoiceSpeakers>> {
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
                !current || !cache.connection.ptr_eq(&Arc::downgrade(&connection.rpc))
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
                    let discovered = discover(&owner).await;
                    // Unlike model discovery, the original speaker operation has
                    // no initial-connection completion fence. A load spanning a
                    // reconnect still completes for its observers. Cache it only
                    // if the schema/config connection remains current, so a later
                    // replacement cannot inherit that retired connection's view.
                    let result = discovered
                        .as_ref()
                        .map(|(catalog, _)| catalog.clone())
                        .map_err(Clone::clone);
                    let current = owner.control.current();
                    {
                        let mut state = owner.state.lock().unwrap();
                        if let Ok((catalog, identity)) = discovered {
                            if current.as_ref().is_some_and(|rpc| {
                                rpc.transport().connected()
                                    && identity.ptr_eq(&Arc::downgrade(&rpc.rpc))
                            }) {
                                state.cache = Some(Cache {
                                    connection: identity,
                                    catalog,
                                    expires_at: (owner.now)() + 60_000,
                                });
                            }
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

async fn call(
    owner: &Inner,
    method: &str,
    params: IvyJson,
) -> IvyResult<(super::VoiceRpc, IvyJson)> {
    let rpc = owner.control.current().ok_or_else(|| {
        IvyError::new(
            "phone_voice_unavailable",
            "Codex Voice transport is unavailable.",
        )
    })?;
    let result = rpc
        .call(
            method,
            params,
            RequestOptions {
                detach_on_timeout: true,
                ..RequestOptions::default()
            },
            Some(15_000),
        )
        .await?;
    Ok((rpc, result))
}

async fn discover(owner: &Inner) -> IvyResult<(Arc<VoiceSpeakers>, Weak<NativeRpc>)> {
    let (_, result) = call(
        owner,
        "thread/realtime/listVoices",
        IvyJson::object([] as [(&str, IvyJson); 0]),
    )
    .await?;
    // The original selects the CURRENT advertised definition after the reply.
    let rpc = owner.control.current().ok_or_else(IvyError::internal)?;
    let definition = rpc
        .rpc
        .catalog()
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
    let voices = result.get("voices").ok_or_else(IvyError::internal)?;
    let (configuration_connection, configuration) = call(
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
    let configured = settings
        .voice
        .as_ref()
        .map(|v| IvyJson::String(v.clone()))
        .or_else(|| configured.cloned());
    let start = rpc
        .rpc
        .catalog()
        .required("clientRequests")?
        .as_array()
        .and_then(|defs| {
            defs.iter()
                .find(|d| d.get("method") == Some(&"thread/realtime/start".into()))
        })
        .ok_or_else(|| {
            IvyError::new(
                "native_method_unavailable",
                "Native Voice startup is unavailable.",
            )
        })?;
    let catalog = select_catalog(voices, configured.as_ref(), |protocol, voice| {
        let params = IvyJson::object([
            ("threadId", "speaker-catalog-discovery".into()),
            ("version", protocol.into()),
            ("voice", voice.clone().into()),
            ("outputModality", "audio".into()),
            ("codexResponseHandoffMode", "commentary".into()),
            ("clientManagedHandoffs", false.into()),
            ("includeStartupContext", false.into()),
            (
                "transport",
                IvyJson::object([
                    ("type", "webrtc".into()),
                    ("sdp", "speaker-catalog-discovery".into()),
                ]),
            ),
            ("prompt", super::owner::instructions::INSTRUCTIONS.into()),
            (
                "initialItems",
                IvyJson::Array(vec![IvyJson::object([
                    ("role", "user".into()),
                    ("text", "Speaker discovery.".into()),
                ])]),
            ),
            ("realtimeStartInstructions", "Speaker discovery.".into()),
        ]);
        let schema = match start.required("inputSchema") {
            Ok(s) => s,
            Err(_) => return false,
        };
        let valid = rpc
            .schemas
            .validate(schema, &params, 6 * 1024 * 1024)
            .is_ok();
        let mut unsupported = params;
        unsupported.insert("version", "unsupported-protocol".into());
        valid
            && rpc
                .schemas
                .validate(schema, &unsupported, 6 * 1024 * 1024)
                .is_err()
    })?;
    let identity = if rpc.same_connection(&configuration_connection) {
        Arc::downgrade(&rpc.rpc)
    } else {
        Weak::new()
    };
    Ok((Arc::new(catalog), identity))
}
fn select_catalog(
    voices: &IvyJson,
    configured: Option<&IvyJson>,
    supports: impl Fn(&str, &JsString) -> bool,
) -> IvyResult<VoiceSpeakers> {
    let invalid = || {
        IvyError::new(
            "phone_speaker_catalog_invalid",
            "No nonempty speaker catalog supports the required native Voice start contract.",
        )
    };
    let mut versions = voices
        .as_object()
        .ok_or_else(invalid)?
        .iter()
        .filter_map(|(key, _)| key.to_utf8().ok())
        .filter(|key| {
            key.strip_prefix('v').is_some_and(|n| {
                !n.starts_with('0') && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
            })
        })
        .collect::<Vec<_>>();
    versions.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| b.cmp(a)));
    for version in versions {
        let values = voices
            .required(&version)?
            .as_array()
            .filter(|ids| ids.len() <= 256)
            .ok_or_else(invalid)?;
        let ids = values
            .iter()
            .map(|id| {
                id.as_js_string()
                    .filter(|id| !id.0.is_empty() && id.0.len() <= 256)
                    .cloned()
                    .ok_or_else(invalid)
            })
            .collect::<IvyResult<Vec<_>>>()?;
        if ids.iter().collect::<HashSet<_>>().len() != ids.len() {
            return Err(invalid());
        }
        if ids.is_empty() || version == "v2" {
            continue;
        }
        let protocol = if version == "v1" {
            "v3"
        } else {
            version.as_str()
        };
        if !ids.iter().all(|id| supports(protocol, id)) {
            continue;
        }
        let default = configured
            .or_else(|| voices.get(format!("defaultV{}", &version[1..]).as_str()))
            .or_else(|| voices.get("defaultV1"));
        let selected = default
            .and_then(IvyJson::as_js_string)
            .filter(|id| ids.contains(id))
            .ok_or_else(|| {
                IvyError::new(
                    if configured.is_some() {
                        "phone_speaker_voice_invalid"
                    } else {
                        "phone_speaker_catalog_invalid"
                    },
                    "Default speaker voice is outside the selected native catalog.",
                )
            })?;
        let default_voice_id = selected.clone();
        return Ok(VoiceSpeakers {
            protocol: protocol.into(),
            catalog_version: version.as_str().into(),
            default_voice_id,
            voices: ids
                .into_iter()
                .map(|id| {
                    Ok(SpeakerVoice {
                        display_name: display_name(&id)?,
                        id,
                    })
                })
                .collect::<IvyResult<_>>()?,
        });
    }
    Err(invalid())
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
