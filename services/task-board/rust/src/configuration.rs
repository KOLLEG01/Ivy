//! Configuration reads and execution selection. Discovery validates a selection;
//! it neither allocates a native Run nor changes a saved execution plan.
use crate::{
    journal_store::{Contracts, JournalStore},
    native_recovery::fresh_context_recovery,
    operations::Store,
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, schema::scalar_types};
use ivy_sdk::{
    operation_ids::new_operation_id,
    rpc::{RequestOptions, RpcClient},
    tools::{BoundToolClient, InterfaceRequirement, ToolCallOptions},
};
use std::{sync::Arc, task::Poll};

pub const CONFIGURATION_NAME: &str = "TaskBoard configuration";

fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn configured_text(value: &Json) -> bool {
    value.as_js_string().is_some_and(|text| !text.0.is_empty())
}
fn nullish<'a>(value: &'a Json, fallback: &'a Json) -> &'a Json {
    if value.is_null() { fallback } else { value }
}
pub fn initial_configuration() -> Json {
    Json::object([
        ("schemaVersion", Json::Number(1.0)),
        (
            "defaults",
            Json::object([
                ("executionRequirement", Json::Null),
                (
                    "nativeOptions",
                    Json::object([
                        ("model", "gpt-6.1-sol".into()),
                        ("reasoningEffort", "xhigh".into()),
                        ("serviceTier", "standard".into()),
                    ]),
                ),
                ("userContact", "chat".into()),
                ("useWorktree", false.into()),
                ("allowParallel", false.into()),
            ]),
        ),
    ])
}

pub async fn configuration_view<S: Store>(store: &S) -> IvyResult<Json> {
    let current = store
        .named("task-board/configuration", CONFIGURATION_NAME, None)
        .await?;
    Ok(Json::object([
        (
            "object",
            current
                .as_ref()
                .map(|d| d.pin.clone())
                .unwrap_or(Json::Null),
        ),
        (
            "configuration",
            current
                .map(|d| d.value)
                .filter(|value| !value.is_null())
                .unwrap_or_else(initial_configuration),
        ),
    ]))
}

pub fn execution_with_defaults(fields: &Json, defaults: &Json) -> Json {
    let options = field(fields, "nativeOptions");
    let inherited = field(defaults, "nativeOptions");
    Json::object([
        (
            "executionRequirement",
            nullish(
                field(fields, "executionRequirement"),
                field(defaults, "executionRequirement"),
            )
            .clone(),
        ),
        (
            "nativeOptions",
            Json::object([
                (
                    "model",
                    nullish(field(options, "model"), field(inherited, "model")).clone(),
                ),
                (
                    "reasoningEffort",
                    nullish(
                        field(options, "reasoningEffort"),
                        field(inherited, "reasoningEffort"),
                    )
                    .clone(),
                ),
                (
                    "serviceTier",
                    nullish(
                        field(options, "serviceTier"),
                        nullish(field(inherited, "serviceTier"), &Json::from("standard")),
                    )
                    .clone(),
                ),
            ]),
        ),
    ])
}

pub async fn effective_execution<S: Store>(store: &S, task: &Json) -> IvyResult<Json> {
    let previous =
        if !field(task, "lastRun").is_null() && !field(task, "primaryResourceRef").is_null() {
            Some(
                store
                    .read("task-board/run", field(task, "lastRun"), true)
                    .await?
                    .value,
            )
        } else {
            fresh_context_recovery(store, task).await?
        };
    let defaults = match previous {
        Some(previous) => {
            let turn = previous.required("plan")?.required("turnStart")?;
            let tier = field(turn, "serviceTier");
            Json::object([
                (
                    "executionRequirement",
                    Json::object([
                        ("kind", "host".into()),
                        (
                            "hostId",
                            previous.required("target")?.required("hostId")?.clone(),
                        ),
                    ]),
                ),
                (
                    "nativeOptions",
                    Json::object([
                        (
                            "model",
                            turn.get("model")
                                .filter(|v| matches!(v, Json::String(_)))
                                .cloned()
                                .unwrap_or(Json::Null),
                        ),
                        (
                            "reasoningEffort",
                            turn.get("effort")
                                .filter(|v| matches!(v, Json::String(_)))
                                .cloned()
                                .unwrap_or(Json::Null),
                        ),
                        (
                            "serviceTier",
                            if tier == &Json::from("fast") || tier == &Json::from("flex") {
                                tier.clone()
                            } else {
                                "standard".into()
                            },
                        ),
                    ]),
                ),
            ])
        }
        None => configuration_view(store)
            .await?
            .required("configuration")?
            .required("defaults")?
            .clone(),
    };
    Ok(execution_with_defaults(task.required("fields")?, &defaults))
}

pub async fn supports_execution_options(
    client: Arc<dyn RpcClient>,
    node: &Json,
    options: &Json,
) -> IvyResult<bool> {
    require_selection(
        configured_text(field(node, "nativeVersion")),
        "task_board_target_unavailable",
        "The selected host has no native version.",
    )?;
    let tools = BoundToolClient::new(
        client.clone(),
        node.required("serviceNodeId")?
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)?,
        &[
            InterfaceRequirement {
                namespace: "agent".into(),
                interface_version: "1.0.0".into(),
            },
            InterfaceRequirement {
                namespace: "codex".into(),
                interface_version: String::from_utf16_lossy(
                    &node
                        .required("nativeVersion")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?
                        .0,
                ),
            },
        ],
        None,
    )?;
    let model = field(options, "model");
    let effort = field(options, "reasoningEffort");
    if configured_text(model) || configured_text(effort) {
        let mut models = Vec::new();
        let mut cursor = None::<JsString>;
        let mut cursors = Vec::new();
        loop {
            let mut args = Json::object([
                ("limit", Json::Number(100.0)),
                ("includeHidden", true.into()),
            ]);
            if let Some(cursor) = cursor.take() {
                args.insert("cursor", Json::String(cursor));
            }
            let operation = new_operation_id(client.clone(), None).await?;
            let page = tools
                .call(
                    &"codex.model/list".into(),
                    args,
                    Some(operation),
                    ToolCallOptions::default(),
                )
                .await?;
            models.extend(
                field(&page, "data")
                    .as_array()
                    .ok_or_else(IvyError::internal)?
                    .iter()
                    .cloned(),
            );
            cursor = field(&page, "nextCursor")
                .as_js_string()
                .filter(|text| !text.0.is_empty())
                .cloned();
            require_selection(
                models.len() <= 1000 && cursor.as_ref().is_none_or(|next| !cursors.contains(next)),
                "task_board_model_unavailable",
                "Model discovery exceeded its complete snapshot limit.",
            )?;
            let Some(next) = &cursor else {
                break;
            };
            cursors.push(next.clone());
        }
        let selected = models.iter().find(|candidate| {
            if configured_text(model) {
                field(candidate, "model") == model || field(candidate, "id") == model
            } else {
                field(candidate, "isDefault").as_bool() == Some(true)
            }
        });
        let Some(selected) = selected else {
            return Ok(false);
        };
        if configured_text(effort)
            && !field(selected, "supportedReasoningEfforts")
                .as_array()
                .ok_or_else(|| IvyError::new("invalid_arguments", "Expected an array."))?
                .iter()
                .any(|value| field(value, "reasoningEffort") == effort)
        {
            return Ok(false);
        }
    }
    let tier = field(options, "serviceTier");
    if configured_text(tier) && tier != &Json::from("standard") {
        let methods = [
            JsString::from("codex.thread/start"),
            JsString::from("codex.turn/start"),
        ];
        let mut futures = methods
            .each_ref()
            .map(|method| Box::pin(tools.binding(method, None)));
        let mut replies = [None, None];
        // Start both reads before exposing a failure, preserving the bound
        // client's ownership of discovery that outlives its public observer.
        let bindings = std::future::poll_fn(|context| {
            for (index, future) in futures.iter_mut().enumerate() {
                if replies[index].is_none() {
                    if let Poll::Ready(reply) = future.as_mut().poll(context) {
                        replies[index] = Some(reply);
                    }
                }
            }
            for reply in &mut replies {
                if reply.as_ref().is_some_and(Result::is_err) {
                    return Poll::Ready(Err(reply.take().unwrap().err().unwrap()));
                }
            }
            if replies.iter().all(Option::is_some) {
                Poll::Ready(Ok(replies
                    .each_mut()
                    .map(|reply| reply.take().unwrap().ok().unwrap())))
            } else {
                Poll::Pending
            }
        })
        .await?;
        for binding in bindings {
            if !scalar_types(
                binding
                    .value()
                    .required("definition")?
                    .required("inputSchema")?,
                &"/serviceTier".into(),
            )?
            .iter()
            .any(|kind| kind == "string")
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}
fn require_selection(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}

/// Domain checks use the journal's mandatory exact validator before any RPC.
pub async fn check_configuration<R: RpcClient + 'static, P: Contracts, C, I>(
    store: &JournalStore<'_, R, P, C, I>,
    value: &Json,
    service_node_id: Option<&JsString>,
    allow_unavailable: bool,
) -> IvyResult<()> {
    store
        .contracts
        .validate_definition("Configuration", value)?;
    let defaults = value.required("defaults")?;
    defaults.required("nativeOptions")?;
    let requirement = field(defaults, "executionRequirement");
    let options = field(defaults, "nativeOptions");
    let host = field(requirement, "kind") == &Json::from("host");
    let mut nodes = Vec::new();
    let mut cursor = None::<JsString>;
    loop {
        let mut args = Json::object([
            ("serviceName", "agent-manager".into()),
            ("limit", Json::Number(200.0)),
        ]);
        if let Some(cursor) = cursor.take() {
            args.insert("cursor", Json::String(cursor));
        }
        let page = store
            .client
            .request("serviceNodes.list", args, RequestOptions::default())
            .await?;
        nodes.extend(
            field(&page, "items")
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .cloned(),
        );
        cursor = field(&page, "nextCursor")
            .as_js_string()
            .filter(|text| !text.0.is_empty())
            .cloned();
        if cursor.is_none() {
            break;
        }
    }
    require_selection(
        !host
            || nodes
                .iter()
                .any(|node| field(node, "hostId") == field(requirement, "hostId")),
        "task_board_target_unavailable",
        "Choose a registered AgentManager host.",
    )?;
    let service_node_id = service_node_id.filter(|text| !text.0.is_empty());
    let eligible: Vec<_> = nodes
        .iter()
        .filter(|node| {
            ["connected", "synced", "ready"]
                .iter()
                .all(|key| field(node, key).as_bool() == Some(true))
                && service_node_id
                    .is_none_or(|id| field(node, "serviceNodeId").as_js_string() == Some(id))
                && (!host || field(node, "hostId") == field(requirement, "hostId"))
        })
        .collect();
    if eligible.is_empty() {
        if allow_unavailable {
            return Ok(());
        }
        require_selection(
            !configured_text(field(options, "model"))
                && !configured_text(field(options, "reasoningEffort"))
                && (!configured_text(field(options, "serviceTier"))
                    || field(options, "serviceTier") == &Json::from("standard")),
            "task_board_target_unavailable",
            "Native execution options cannot be verified while the selected AgentManager is unavailable. Retry when the host is ready.",
        )?;
        return Ok(());
    }
    for node in eligible {
        if supports_execution_options(store.client.clone(), node, options).await? {
            return Ok(());
        }
    }
    let mut message = JsString::from("No ready AgentManager");
    if host {
        message.0.extend(" on ".encode_utf16());
        message.0.extend(
            field(requirement, "hostId")
                .as_js_string()
                .ok_or_else(|| IvyError::new("invalid_arguments", "Expected a string."))?
                .0
                .iter()
                .copied(),
        );
    }
    for (prefix, key, fallback) in [
        (" supports model=", "model", "default"),
        (", reasoning=", "reasoningEffort", "default"),
        (", speed=", "serviceTier", "standard"),
    ] {
        message.0.extend(prefix.encode_utf16());
        let selected = field(options, key);
        if selected.is_null() {
            message.0.extend(fallback.encode_utf16());
        } else {
            message.0.extend(
                selected
                    .as_js_string()
                    .ok_or_else(|| IvyError::new("invalid_arguments", "Expected a string."))?
                    .0
                    .iter()
                    .copied(),
            );
        }
    }
    message
        .0
        .extend(". Choose exact values from the host's model/list catalog.".encode_utf16());
    Err(IvyError::new_js(
        "task_board_model_unavailable".into(),
        message,
    ))
}
