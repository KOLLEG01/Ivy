//! Configuration reads and execution selection. Discovery validates a selection;
//! it neither allocates a native Run nor changes a saved execution plan.
use crate::{
    journal_store::{Contracts, JournalStore},
    native_recovery::fresh_context_recovery,
    operations::Store,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, json::js_string, schema::scalar_types,
};
use ivy_sdk::{
    operation_ids::new_operation_id,
    rpc::{RequestOptions, RpcClient},
    tools::{BoundToolClient, InterfaceRequirement, ToolCallOptions},
};
use std::{collections::HashSet, future::Future, sync::Arc, task::Poll};

pub const CONFIGURATION_NAME: &str = "TaskBoard configuration";

fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(value) => *value,
        Json::Number(value) => *value != 0.0 && !value.is_nan(),
        Json::String(value) => !value.0.is_empty(),
        _ => true,
    }
}
fn text(value: &Json, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn nullish<'a>(value: &'a Json, fallback: &'a Json) -> &'a Json {
    if value.is_null() { fallback } else { value }
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
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
    let previous = if truthy(field(task, "lastRun")) && truthy(field(task, "primaryResourceRef")) {
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
    require(
        truthy(field(node, "nativeVersion")),
        "task_board_target_unavailable",
        "The selected host has no native version.",
    )?;
    let tools = BoundToolClient::new(
        client.clone(),
        text(node, "serviceNodeId")?,
        &[
            InterfaceRequirement {
                namespace: "agent".into(),
                interface_version: "1.0.0".into(),
            },
            InterfaceRequirement {
                namespace: "codex".into(),
                // The SDK admits only ASCII dotted versions. Unpaired units
                // must reach its same interface-admission error as in JS.
                interface_version: String::from_utf16_lossy(&text(node, "nativeVersion")?.0),
            },
        ],
        None,
    )?;
    if truthy(field(options, "model")) || truthy(field(options, "reasoningEffort")) {
        let mut models = Vec::new();
        let mut next = Json::Null;
        let mut cursors = HashSet::new();
        loop {
            let mut args = Json::object([
                ("limit", Json::Number(100.0)),
                ("includeHidden", true.into()),
            ]);
            if truthy(&next) {
                args.insert("cursor", next);
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
            models.extend_from_slice(
                page.required("data")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?,
            );
            next = field(&page, "nextCursor").clone();
            let cursor = next.as_js_string();
            require(
                models.len() <= 1000
                    && (!truthy(&next)
                        || !cursors.contains(cursor.ok_or_else(IvyError::internal)?)),
                "task_board_model_unavailable",
                "Model discovery exceeded its complete snapshot limit.",
            )?;
            if !truthy(&next) {
                break;
            }
            cursors.insert(cursor.unwrap().clone());
        }
        let model = models.iter().find(|model| {
            if truthy(field(options, "model")) {
                field(model, "model") == field(options, "model")
                    || field(model, "id") == field(options, "model")
            } else {
                truthy(field(model, "isDefault"))
            }
        });
        let Some(model) = model else {
            return Ok(false);
        };
        if truthy(field(options, "reasoningEffort"))
            && !model
                .required("supportedReasoningEfforts")?
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .any(|effort| field(effort, "reasoningEffort") == field(options, "reasoningEffort"))
        {
            return Ok(false);
        }
    }
    if truthy(field(options, "serviceTier"))
        && field(options, "serviceTier") != &Json::from("standard")
    {
        let names = [
            JsString::from("codex.thread/start"),
            JsString::from("codex.turn/start"),
        ];
        let mut futures = names
            .iter()
            .map(|name| Box::pin(tools.binding(name, None)))
            .collect::<Vec<_>>();
        let mut replies = [None, None];
        // Start both lookups before exposing a failure, like Promise.all. The
        // shared bound client keeps each started discovery alive independently.
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
                    .map(|reply| reply.take().unwrap().unwrap())))
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
    let requirement = field(defaults, "executionRequirement");
    let options = defaults.required("nativeOptions")?;
    let host = field(requirement, "kind") == &Json::from("host");
    let mut nodes = Vec::new();
    let mut cursor = Json::Null;
    loop {
        let mut args = Json::object([
            ("serviceName", "agent-manager".into()),
            ("limit", Json::Number(200.0)),
        ]);
        if truthy(&cursor) {
            args.insert("cursor", cursor);
        }
        let page = store
            .client
            .request("serviceNodes.list", args, RequestOptions::default())
            .await?;
        nodes.extend_from_slice(
            page.required("items")?
                .as_array()
                .ok_or_else(IvyError::internal)?,
        );
        cursor = field(&page, "nextCursor").clone();
        if !truthy(&cursor) {
            break;
        }
    }
    require(
        !host
            || nodes
                .iter()
                .any(|node| field(node, "hostId") == field(requirement, "hostId")),
        "task_board_target_unavailable",
        "Choose a registered AgentManager host.",
    )?;
    let eligible = nodes
        .iter()
        .filter(|node| {
            truthy(field(node, "connected"))
                && truthy(field(node, "synced"))
                && truthy(field(node, "ready"))
                && service_node_id
                    .filter(|id| !id.0.is_empty())
                    .is_none_or(|id| {
                        node.get("serviceNodeId").and_then(Json::as_js_string) == Some(id)
                    })
                && (!host || field(node, "hostId") == field(requirement, "hostId"))
        })
        .collect::<Vec<_>>();
    if eligible.is_empty() {
        if allow_unavailable {
            return Ok(());
        }
        return require(
            !truthy(field(options, "model"))
                && !truthy(field(options, "reasoningEffort"))
                && (!truthy(field(options, "serviceTier"))
                    || field(options, "serviceTier") == &Json::from("standard")),
            "task_board_target_unavailable",
            "Native execution options cannot be verified while the selected AgentManager is unavailable. Retry when the host is ready.",
        );
    }
    for node in eligible {
        if supports_execution_options(store.client.clone(), node, options).await? {
            return Ok(());
        }
    }
    let mut message = JsString::from("No ready AgentManager");
    if host {
        message.0.extend(" on ".encode_utf16());
        message.0.extend(js_string(field(requirement, "hostId")).0);
    }
    for (prefix, key, fallback) in [
        (" supports model=", "model", "default"),
        (", reasoning=", "reasoningEffort", "default"),
        (", speed=", "serviceTier", "standard"),
    ] {
        message.0.extend(prefix.encode_utf16());
        message
            .0
            .extend(js_string(nullish(field(options, key), &Json::from(fallback))).0);
    }
    message
        .0
        .extend(". Choose exact values from the host's model/list catalog.".encode_utf16());
    Err(IvyError::new_js(
        "task_board_model_unavailable".into(),
        message,
    ))
}
