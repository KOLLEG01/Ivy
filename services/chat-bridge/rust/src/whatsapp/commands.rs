//! WhatsApp command policy; Main, native session and desktop retain their owners.
use super::{
    command_native::{CommandParameters, NativeCommands, ReadyParameters},
    journal::Inbox,
    journal_owner::Journal,
    messages::Translator,
};
use crate::{
    chat_main::ChatMain,
    main_context::object_options,
    store::{Document, Pin, Reference},
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, json::parse};
use ivy_sdk::{
    native_owner::{GuardFuture, NativeDispatchGuard},
    operation_ids::scoped_operation_id,
    rpc::RequestFuture,
};
use std::{future::Future, pin::Pin as FuturePin};

pub type CommandFuture<'a> =
    FuturePin<Box<dyn Future<Output = IvyResult<Option<JsString>>> + Send + 'a>>;
pub trait CommandMain: Send + Sync {
    fn find(&self) -> RequestFuture<'_>;
    fn binding(&self, reference: Json) -> RequestFuture<'_>;
    fn plan(&self) -> RequestFuture<'_>;
    fn expected(&self, sender: &JsString) -> Json;
    fn operation_id(&self, scope: Json) -> RequestFuture<'_>;
    fn create<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a>;
}
fn document(value: Document) -> Json {
    Json::object([("pin", value.pin.json()), ("value", value.value)])
}
impl CommandMain for ChatMain<'_> {
    fn find(&self) -> RequestFuture<'_> {
        Box::pin(async {
            Ok(ChatMain::find(self)
                .await?
                .map(document)
                .unwrap_or(Json::Null))
        })
    }
    fn binding(&self, reference: Json) -> RequestFuture<'_> {
        Box::pin(async move {
            let pin = Pin {
                object_id: reference.required("objectId")?.as_str()?,
                revision: reference
                    .required("revision")?
                    .as_f64()
                    .ok_or_else(IvyError::internal)? as i64,
            };
            Ok(document(
                self.store()
                    .read("chat-bridge/binding", Reference::Pinned(&pin))
                    .await
                    .map_err(|e| e.into_ivy())?,
            ))
        })
    }
    fn plan(&self) -> RequestFuture<'_> {
        Box::pin(async { self.native.resolve_plan() })
    }
    fn expected(&self, sender: &JsString) -> Json {
        self.admission().expected(sender)
    }
    fn operation_id(&self, scope: Json) -> RequestFuture<'_> {
        Box::pin(async move {
            Ok(Json::String(
                scoped_operation_id(self.store().client.clone(), &scope).await?,
            ))
        })
    }
    fn create<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a> {
        Box::pin(ChatMain::create(self, sender, request))
    }
}
pub trait DesktopCommands: Send + Sync {
    fn run<'a>(&'a self, action: &'a JsString, key: &'a str) -> RequestFuture<'a>;
}

pub struct Command {
    pub name: JsString,
    pub args: JsString,
}
pub fn parse_command(text: &JsString) -> Option<Command> {
    let decoded = String::from_utf16_lossy(&text.0);
    let trimmed = decoded.trim();
    let command = trimmed.strip_prefix('/')?;
    let end = command
        .find(|value: char| !value.is_ascii_alphabetic())
        .unwrap_or(command.len());
    if end == 0
        || command[end..]
            .chars()
            .next()
            .is_some_and(|value| !value.is_whitespace())
    {
        return None;
    }
    let args = command[end..].trim();
    // Parse the ASCII header as Rust text while retaining the argument's
    // original wire encoding, including text saved by earlier clients.
    let argument_end = (decoded.len() - decoded.trim_start().len()) + trimmed.len();
    let argument_start = argument_end - args.len();
    let start = decoded[..argument_start].encode_utf16().count();
    let encoded_end = decoded[..argument_end].encode_utf16().count();
    Some(Command {
        name: command[..end].to_ascii_lowercase().into(),
        args: JsString(text.0[start..encoded_end].to_vec()),
    })
}
fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn nullish(values: &[Option<&Json>]) -> Json {
    values
        .iter()
        .find_map(|v| v.filter(|v| !v.is_null()).cloned())
        .unwrap_or(Json::Null)
}
fn any(value: &JsString, choices: &[&str]) -> bool {
    choices.iter().any(|v| value == &JsString::from(*v))
}
fn defaults(value: &JsString) -> bool {
    any(value, &["default", "reset", "auto"])
}
fn effort(value: &JsString) -> bool {
    any(
        value,
        &[
            "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
        ],
    )
}
fn string(value: Option<&Json>) -> JsString {
    value
        .and_then(Json::as_js_string)
        .cloned()
        .unwrap_or_default()
}
fn append(out: &mut String, text: &JsString) -> IvyResult<()> {
    out.push_str(&text.to_utf8()?);
    Ok(())
}
fn split_space(value: &JsString) -> Vec<JsString> {
    value
        .to_utf8()
        .unwrap_or_default()
        .split_whitespace()
        .map(Into::into)
        .collect()
}
fn remove(value: &mut Json, key: &str) {
    if let Json::Object(fields) = value {
        fields.retain(|(k, _)| k != &JsString::from(key));
    }
}

pub struct WhatsAppCommands<'a> {
    pub main: &'a dyn CommandMain,
    pub journal: Journal,
    pub native: &'a dyn NativeCommands,
    pub desktop: &'a dyn DesktopCommands,
    pub translator: Translator,
}
impl WhatsAppCommands<'_> {
    fn t(&self, key: &str, parameters: Json) -> IvyResult<JsString> {
        self.translator.translate(key, Some(&parameters))
    }
    fn check(&self, ok: bool, code: &str, key: &str, parameters: Json) -> IvyResult<()> {
        let message = self.t(key, parameters)?;
        if ok {
            Ok(())
        } else {
            Err(IvyError::new(code, "").with_js_message(message))
        }
    }
    pub fn preferences(&self) -> IvyResult<Json> {
        let value = self.journal.get("preferences")?;
        Ok(if value.is_null() {
            Json::object([("commentary", false.into())])
        } else if value.as_object().is_none() {
            return Err(IvyError::internal());
        } else {
            value
        })
    }
    pub fn turn_options(&self) -> IvyResult<Json> {
        let mut saved = object_options(&self.preferences()?);
        remove(&mut saved, "commentary");
        let model = nullish(&[saved.get("model"), Some(&Json::from("gpt-5.6-terra"))]);
        let effort = nullish(&[saved.get("effort"), Some(&Json::from("medium"))]);
        saved.insert("model", model);
        saved.insert("effort", effort);
        Ok(saved)
    }
    pub fn run<'a>(&'a self, row: &'a Inbox, text: &'a JsString) -> CommandFuture<'a> {
        Box::pin(async move {
            let parsed = parse_command(text);
            self.check(
                parsed.is_some(),
                "invalid_arguments",
                "command.invalid",
                Json::Null,
            )?;
            let Command { name, args } = parsed.unwrap();
            let normalized: JsString = String::from_utf16_lossy(&args.0).to_lowercase().into();
            let empty = args.0.is_empty();
            if any(&name, &["help", "start"]) {
                return Ok(Some(self.t("help", Json::Null)?));
            }
            if name == JsString::from("new") {
                self.check(empty, "invalid_arguments", "new.usage", Json::Null)?;
                let prior = row
                    .request
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .map(|s| parse(s, usize::MAX))
                    .transpose()?;
                let request = if let Some(prior) = prior.filter(|value| !value.is_null()) {
                    prior
                } else {
                    let main = self.main.find().await?;
                    let request = Json::object([
                        ("action", "createMain".into()),
                        (
                            "operationId",
                            self.main
                                .operation_id(Json::Array(vec![
                                    "whatsapp-new".into(),
                                    row.id.clone().into(),
                                ]))
                                .await?,
                        ),
                        (
                            "expectedBridge",
                            self.main.expected(&row.sender.clone().into()),
                        ),
                        (
                            "expectedMainRevision",
                            field(field(&main, "pin"), "revision").clone(),
                        ),
                        ("reason", Json::String(self.t("new.reason", Json::Null)?)),
                    ]);
                    self.journal.access(|j| j.save_request(&row.id, &request))?;
                    request
                };
                let result = self
                    .main
                    .create(&row.sender.clone().into(), &request)
                    .await?;
                if result.get("phase") == Some(&"failed".into()) {
                    return Ok(Some(self.t(
                        "new.failed",
                        Json::object([(
                            "code",
                            nullish(&[
                                field(&result, "error").get("code"),
                                Some(&"unknown".into()),
                            ]),
                        )]),
                    )?));
                }
                if result.get("phase") != Some(&"succeeded".into())
                    || field(&result, "outcome").get("action") != Some(&"createMain".into())
                {
                    return Ok(None);
                }
                let binding = field(field(&result, "outcome"), "binding");
                self.journal.set(
                    &format!(
                        "listener-new:{}",
                        binding.required("object")?.required("objectId")?.as_str()?
                    ),
                    &true.into(),
                )?;
                self.native.invalidate();
                return Ok(Some(
                    self.t(
                        "new.success",
                        Json::object([(
                            "id",
                            binding
                                .required("data")?
                                .required("primary")?
                                .required("nativeId")?
                                .clone(),
                        )]),
                    )?,
                ));
            }
            if name == JsString::from("steer") {
                self.check(
                    !empty && char::decode_utf16(args.0.iter().copied()).count() <= 65536,
                    "invalid_arguments",
                    "steer.usage",
                    Json::Null,
                )?;
                let factory = SteerParameters {
                    commands: self,
                    row,
                    args,
                };
                let guard = SteerGuard {
                    commands: self,
                    row,
                };
                let result = self
                    .native
                    .command(&row.id, "turn/steer", &factory, Some(&guard))
                    .await?;
                return Ok(if result.is_null() {
                    None
                } else {
                    Some(self.t("steer.success", Json::Null)?)
                });
            }
            if name == JsString::from("desktop") {
                return Ok(Some(
                    self.desktop
                        .run(&args, &row.id)
                        .await?
                        .into_js_string()
                        .ok_or_else(IvyError::internal)?,
                ));
            }
            if any(&name, &["status", "task"])
                || any(&name, &["model", "reasoning", "fast", "commentary"]) && empty
            {
                self.check(
                    empty || name != JsString::from("task"),
                    "invalid_arguments",
                    "task.status_only",
                    Json::Null,
                )?;
                let main = self.main.find().await?;
                let binding = if !(field(field(&main, "value"), "binding")).is_null() {
                    self.main
                        .binding(field(field(&main, "value"), "binding").clone())
                        .await?
                } else {
                    Json::Null
                };
                let plan = self.main.plan().await?;
                let start = field(&plan, "turnStart");
                let mut p = object_options(start);
                for source in [self.preferences()?, self.turn_options()?] {
                    for (k, v) in object_options(&source).as_object().unwrap() {
                        p.insert(k.clone(), v.clone());
                    }
                }
                let settings = field(field(start, "collaborationMode"), "settings");
                let thread_model = field(&plan, "threadStart")
                    .get("model")
                    .filter(|v| v.as_js_string().is_some());
                return Ok(Some(
                    self.t(
                        "status",
                        Json::object([
                            (
                                "main",
                                nullish(&[
                                    field(field(&binding, "value"), "primary").get("nativeId"),
                                    Some(&Json::String(self.t("status.pending", Json::Null)?)),
                                ]),
                            ),
                            (
                                "queue",
                                Json::Number(
                                    field(field(&main, "value"), "queue")
                                        .as_array()
                                        .map_or(0, |v| v.len())
                                        as f64,
                                ),
                            ),
                            (
                                "model",
                                nullish(&[
                                    p.get("model"),
                                    settings.get("model"),
                                    thread_model,
                                    Some(&Json::String(self.t("status.default", Json::Null)?)),
                                ]),
                            ),
                            (
                                "reasoning",
                                nullish(&[
                                    p.get("effort"),
                                    settings.get("reasoning_effort"),
                                    Some(&Json::String(self.t("status.default", Json::Null)?)),
                                ]),
                            ),
                            (
                                "fast",
                                Json::String(self.t(
                                    if p.get("serviceTier") == Some(&"fast".into()) {
                                        "status.enabled"
                                    } else {
                                        "status.disabled"
                                    },
                                    Json::Null,
                                )?),
                            ),
                            (
                                "commentary",
                                Json::String(self.t(
                                    if (field(&p, "commentary")).as_bool().unwrap_or(false) {
                                        "status.enabled"
                                    } else {
                                        "status.disabled"
                                    },
                                    Json::Null,
                                )?),
                            ),
                        ]),
                    )?,
                ));
            }
            if any(&name, &["commentary", "fast"]) {
                let on = any(&normalized, &["on", "an", "true", "1", "yes", "ja"]);
                let off = any(&normalized, &["off", "aus", "false", "0", "no", "nein"]);
                self.check(
                    on || off,
                    "invalid_arguments",
                    "toggle.usage",
                    Json::object([("name", Json::String(name.clone()))]),
                )?;
                let mut preferences = self.preferences()?;
                match &mut preferences {
                    Json::Object(_) => preferences.insert(
                        if name == JsString::from("commentary") {
                            "commentary"
                        } else {
                            "serviceTier"
                        },
                        if name == JsString::from("commentary") {
                            on.into()
                        } else if on {
                            "fast".into()
                        } else {
                            Json::Null
                        },
                    ),
                    Json::Array(_) => {}
                    _ => return Err(IvyError::internal()),
                }
                self.journal.set("preferences", &preferences)?;
                return Ok(Some(self.t(
                    "toggle.result",
                    Json::object([
                        (
                            "setting",
                            Json::String(self.t(
                                if name == JsString::from("fast") {
                                    "toggle.fast"
                                } else {
                                    "toggle.commentary"
                                },
                                Json::Null,
                            )?),
                        ),
                        (
                            "state",
                            Json::String(self.t(
                                if on {
                                    "status.enabled"
                                } else {
                                    "status.disabled"
                                },
                                Json::Null,
                            )?),
                        ),
                    ]),
                )?));
            }
            if name == JsString::from("reasoning") {
                self.check(
                    defaults(&normalized) || effort(&normalized),
                    "invalid_arguments",
                    "reasoning.invalid",
                    Json::Null,
                )?;
                let mut p = object_options(&self.preferences()?);
                p.insert(
                    "effort",
                    if defaults(&normalized) {
                        Json::Null
                    } else {
                        Json::String(normalized.clone())
                    },
                );
                self.journal.set("preferences", &p)?;
                return Ok(Some(self.t(
                    "reasoning.result",
                    Json::object([("effort", Json::String(normalized))]),
                )?));
            }
            if any(&name, &["models", "model"]) {
                let result = self
                    .native
                    .command(
                        &row.id,
                        "model/list",
                        &ReadyParameters(Json::object([
                            ("limit", Json::Number(100.0)),
                            ("includeHidden", false.into()),
                        ])),
                        None,
                    )
                    .await?;
                if result.is_null() {
                    return Ok(None);
                }
                self.check(
                    result.get("data").and_then(Json::as_array).is_some(),
                    "native_read_failed",
                    "models.missing",
                    Json::Null,
                )?;
                let models = result.required("data")?.as_array().unwrap();
                if name == JsString::from("models") {
                    let mut text = String::new();
                    for (index, model) in models.iter().enumerate() {
                        if index > 0 {
                            append(&mut text, &"\n".into())?;
                        }
                        append(
                            &mut text,
                            &string(
                                model
                                    .get("model")
                                    .filter(|value| {
                                        value.as_js_string().is_some_and(|text| !text.0.is_empty())
                                    })
                                    .or_else(|| model.get("id")),
                            ),
                        )?;
                        append(&mut text, &" — ".into())?;
                        append(&mut text, &string(model.get("displayName")))?;
                    }
                    return Ok(Some(text.into()));
                }
                let parts = split_space(&normalized);
                let model = parts.first();
                let selected_effort = parts.get(1);
                self.check(
                    model.is_some_and(|v| !v.0.is_empty())
                        && parts.len() <= 2
                        && selected_effort.is_none_or(effort),
                    "invalid_arguments",
                    "model.usage",
                    Json::Null,
                )?;
                let model = model.unwrap();
                let mut selected = None;
                for candidate in models {
                    if candidate.required("model")?.as_str()?.to_lowercase() == model.to_utf8()?
                        || candidate.required("id")?.as_str()?.to_lowercase() == model.to_utf8()?
                    {
                        selected = Some(candidate);
                        break;
                    }
                }
                self.check(
                    defaults(model) || selected.is_some(),
                    "invalid_arguments",
                    "model.unavailable",
                    Json::Null,
                )?;
                let supported = selected.and_then(|v| v.get("supportedReasoningEfforts"));
                let allowed = match (selected_effort, supported.filter(|v| !(v).is_null())) {
                    (Some(effort), Some(values)) => values
                        .as_array()
                        .ok_or_else(IvyError::internal)?
                        .iter()
                        .any(|v| v.get("reasoningEffort") == Some(&Json::String(effort.clone()))),
                    _ => true,
                };
                self.check(allowed, "invalid_arguments", "model.reasoning", Json::Null)?;
                let mut p = object_options(&self.preferences()?);
                p.insert(
                    "model",
                    if defaults(model) {
                        Json::Null
                    } else {
                        selected
                            .unwrap()
                            .get("model")
                            .filter(|value| {
                                value.as_js_string().is_some_and(|text| !text.0.is_empty())
                            })
                            .or_else(|| selected.unwrap().get("id"))
                            .ok_or_else(IvyError::internal)?
                            .clone()
                    },
                );
                if let Some(effort) = selected_effort {
                    p.insert("effort", Json::String(effort.clone()));
                }
                self.journal.set("preferences", &p)?;
                return Ok(Some(self.t(
                    "model.result",
                    Json::object([("model", Json::String(model.clone()))]),
                )?));
            }
            if any(&name, &["threads", "tasks"]) {
                let limit = if empty {
                    10.0
                } else {
                    args.to_utf8()?.parse::<f64>().unwrap_or(f64::NAN)
                };
                self.check(
                    limit.is_finite() && limit.fract() == 0.0 && (1.0..=25.0).contains(&limit),
                    "invalid_arguments",
                    "threads.usage",
                    Json::Null,
                )?;
                let result = self
                    .native
                    .command(
                        &row.id,
                        "thread/list",
                        &ReadyParameters(Json::object([
                            ("limit", Json::Number(limit)),
                            ("sortKey", "updated_at".into()),
                        ])),
                        None,
                    )
                    .await?;
                if result.is_null() {
                    return Ok(None);
                }
                let mut text = self.t("threads.header", Json::Null)?.to_utf8()?;
                append(&mut text, &"\n".into())?;
                for (index, row) in result
                    .required("data")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?
                    .iter()
                    .enumerate()
                {
                    if index > 0 {
                        append(&mut text, &"\n".into())?;
                    }
                    append(&mut text, &string(row.get("id")))?;
                    append(&mut text, &" — ".into())?;
                    append(
                        &mut text,
                        &string(Some(&nullish(&[
                            row.get("name"),
                            row.get("preview"),
                            Some(&"".into()),
                        ]))),
                    )?;
                }
                return Ok(Some(text.into()));
            }
            Ok(Some(self.t(
                "command.unknown",
                Json::object([("help", Json::String(self.t("help", Json::Null)?))]),
            )?))
        })
    }
}

struct SteerParameters<'a, 'b> {
    commands: &'a WhatsAppCommands<'b>,
    row: &'a Inbox,
    args: JsString,
}
impl CommandParameters for SteerParameters<'_, '_> {
    fn build(&self) -> RequestFuture<'_> {
        Box::pin(async {
            let main = self.commands.main.find().await?;
            let reference = field(field(&main, "value"), "binding");
            self.commands.check(
                !(reference).is_null(),
                "chat_main_missing",
                "steer.first",
                Json::Null,
            )?;
            let binding = self.commands.main.binding(reference.clone()).await?;
            let thread = binding
                .required("value")?
                .required("primary")?
                .required("nativeId")?
                .clone();
            let page = self
                .commands
                .native
                .read(
                    "thread/turns/list",
                    Json::object([
                        ("threadId", thread.clone()),
                        ("limit", Json::Number(1.0)),
                        ("itemsView", "notLoaded".into()),
                        ("sortDirection", "desc".into()),
                    ]),
                )
                .await?;
            let turn = page
                .required("data")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .first();
            self.commands.check(
                turn.is_some_and(|t| t.get("status") == Some(&"inProgress".into())),
                "chat_no_active_turn",
                "steer.inactive",
                Json::Null,
            )?;
            self.commands.journal.set(
                &format!("steer-binding:{}", self.row.id),
                binding.required("pin")?.required("objectId")?,
            )?;
            let mut params = Json::object([("threadId", thread)]);
            if let Some(id) = turn.unwrap().get("id") {
                params.insert("expectedTurnId", id.clone());
            }
            for (key, value) in [
                (
                    "input",
                    Json::Array(vec![Json::object([
                        ("type", "text".into()),
                        ("text", Json::String(self.args.clone())),
                        ("text_elements", Json::Array(vec![])),
                    ])]),
                ),
                (
                    "clientUserMessageId",
                    format!("wa-steer-{}", self.row.id).into(),
                ),
            ] {
                params.insert(key, value);
            }
            Ok(params)
        })
    }
}
struct SteerGuard<'a, 'b> {
    commands: &'a WhatsAppCommands<'b>,
    row: &'a Inbox,
}
impl NativeDispatchGuard for SteerGuard<'_, '_> {
    fn check(&self) -> GuardFuture<'_> {
        Box::pin(async {
            let main = self.commands.main.find().await?;
            self.commands.check(
                field(field(&main, "value"), "binding").get("objectId")
                    == Some(
                        &self
                            .commands
                            .journal
                            .get(&format!("steer-binding:{}", self.row.id))?,
                    ),
                "chat_binding_mismatch",
                "steer.changed",
                Json::Null,
            )
        })
    }
}
