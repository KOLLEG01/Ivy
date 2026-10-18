//! Native management handlers. Service startup composes the remaining handlers
//! before publishing the complete registry through the shared connection.
use crate::{
    interactions::NativeInteractions,
    native_discovery::discover_native_tools,
    native_dispatch::{NativeDispatch, caller_identity},
    native_journal::JournalClock,
    native_operations::NativeOperations,
    registry::agent_registry,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::{hash_json, tool_definition_hash},
    json::try_js_string,
    limits::{
        MANAGEMENT_FRAME_BYTES, NATIVE_ANSWER_FRAME_BYTES, NATIVE_FRAME_BYTES,
        NATIVE_REQUEST_FRAME_BYTES,
    },
};
use ivy_host_runtime::{
    codex_admission::Priority,
    codex_rpc::{HookFuture, NativeRpc, RequestOptions},
};
use ivy_sdk::service_connection::{InvocationContext, ServiceFuture, ToolHandler};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, atomic::AtomicBool},
};

pub struct NativeManagementOwner {
    pub service_node_id: JsString,
    pub epoch: JsString,
    pub catalog: IvyJson,
    pub rpc: NativeRpc,
    pub closing: Arc<AtomicBool>,
    pub platform: String,
}

struct Inner {
    owner: NativeManagementOwner,
    catalog_hash: String,
    registry: IvyJson,
    definitions: HashMap<JsString, IvyJson>,
    dispatch: NativeDispatch,
    operations: NativeOperations,
    interactions: NativeInteractions,
    clock: Arc<JournalClock>,
}

#[derive(Clone)]
pub struct NativeManagement(Arc<Inner>);

const TOOLS: &[&str] = &[
    "catalog",
    "discover",
    "frameLimits",
    "prevent",
    "invoke",
    "interact",
    "interaction",
    "read",
    "operation",
    "inputs",
    "inputDefinition",
    "answer",
];

fn require(test: bool, code: &str, message: &str) -> IvyResult<()> {
    if test {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn property_string(args: &IvyJson, key: &str) -> IvyResult<JsString> {
    args.get(key)
        .map(try_js_string)
        .unwrap_or_else(|| Ok("undefined".into()))
}
fn array<'a>(value: &'a IvyJson, key: &str) -> &'a [IvyJson] {
    value.get(key).and_then(IvyJson::as_array).unwrap()
}

impl NativeManagement {
    pub fn new(
        owner: NativeManagementOwner,
        operations: NativeOperations,
        interactions: NativeInteractions,
    ) -> IvyResult<Self> {
        Self::with_clock(
            owner,
            operations,
            interactions,
            Arc::new(|| {
                chrono::Utc::now()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    .into()
            }),
        )
    }

    pub fn with_clock(
        owner: NativeManagementOwner,
        operations: NativeOperations,
        interactions: NativeInteractions,
        clock: Arc<JournalClock>,
    ) -> IvyResult<Self> {
        let registry = agent_registry(&owner.catalog)?;
        let mut definitions = HashMap::new();
        for tool in array(&array(&registry, "namespaces")[0], "tools") {
            // The original Map uses the last definition for duplicate names.
            definitions.insert(
                tool.get("nativeMethod")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .clone(),
                tool_definition_hash(tool)?.into(),
            );
        }
        let dispatch = NativeDispatch::new(
            owner.rpc.clone(),
            owner.epoch.clone(),
            operations.clone(),
            owner.closing.clone(),
        );
        Ok(Self(Arc::new(Inner {
            catalog_hash: hash_json(&owner.catalog)?,
            owner,
            registry,
            definitions,
            dispatch,
            operations,
            interactions,
            clock,
        })))
    }

    pub fn registry(&self) -> &IvyJson {
        &self.0.registry
    }

    /// Only implemented native handlers are returned. The shared connection
    /// refuses registry publication until every other declared handler is added.
    pub fn handlers(&self) -> HashMap<JsString, ToolHandler> {
        TOOLS
            .iter()
            .map(|name| {
                let owner = self.clone();
                let name = *name;
                let handler: ToolHandler = Arc::new(move |args, context| {
                    let owner = owner.clone();
                    Box::pin(async move { owner.call(name, args, context).await })
                        as ServiceFuture<IvyJson>
                });
                (format!("agent.{name}").into(), handler)
            })
            .collect()
    }

    fn bound(&self, args: &IvyJson, context: &InvocationContext, require_method: bool) -> bool {
        let method = args.get("method").and_then(IvyJson::as_js_string);
        let expected = method.and_then(|method| self.0.definitions.get(method));
        args.get("operationId") == context.operation_id.clone().map(IvyJson::String).as_ref()
            && args.get("nativeVersion") == self.0.owner.catalog.get("version")
            && (!require_method
                || method.is_some_and(|method| self.0.definitions.contains_key(method)))
            && expected == args.get("expectedDefinitionHash")
    }

    async fn call(
        &self,
        name: &str,
        args: IvyJson,
        context: InvocationContext,
    ) -> IvyResult<IvyJson> {
        let state = &self.0;
        match name {
            "catalog" => {
                state.dispatch.available()?;
                Ok(state.owner.catalog.clone())
            }
            "discover" => {
                state.dispatch.available()?;
                discover_native_tools(
                    &state.owner.catalog,
                    array(&array(&state.registry, "namespaces")[0], "tools"),
                    &args,
                    &state.owner.platform,
                )
            }
            "frameLimits" => {
                let epoch = state.operations.journal().lock().unwrap().epoch().cloned();
                Ok(IvyJson::object([
                    (
                        "serviceNodeId",
                        IvyJson::String(state.owner.service_node_id.clone()),
                    ),
                    (
                        "nativeVersion",
                        state.owner.catalog.required("version")?.clone(),
                    ),
                    (
                        "nativeExecutableHash",
                        state
                            .owner
                            .catalog
                            .required("nativeExecutableHash")?
                            .clone(),
                    ),
                    ("catalogHash", state.catalog_hash.clone().into()),
                    ("epoch", epoch.map(IvyJson::String).unwrap_or(IvyJson::Null)),
                    (
                        "requestFrameBytes",
                        IvyJson::Number(NATIVE_REQUEST_FRAME_BYTES as f64),
                    ),
                    (
                        "receivedFrameBytes",
                        IvyJson::Number(NATIVE_FRAME_BYTES as f64),
                    ),
                    (
                        "answerFrameBytes",
                        IvyJson::Number(NATIVE_ANSWER_FRAME_BYTES as f64),
                    ),
                    (
                        "managementFrameBytes",
                        IvyJson::Number(MANAGEMENT_FRAME_BYTES as f64),
                    ),
                ]))
            }
            "prevent" => {
                state.dispatch.available()?;
                require(
                    context.operation_id.as_ref().is_none_or(|outer| {
                        outer.0.is_empty()
                            || args.get("operationId").and_then(IvyJson::as_js_string)
                                == Some(outer)
                    }),
                    "mutation_conflict",
                    "Outer prevention identity must match the original native operation.",
                )?;
                require(
                    args.get("nativeVersion") == state.owner.catalog.get("version"),
                    "native_version_mismatch",
                    "Prevention requires the exact installed native version.",
                )?;
                let method = args
                    .required("method")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?;
                require(
                    state.definitions.contains_key(method)
                        && state.definitions.get(method) == args.get("expectedDefinitionHash"),
                    "native_definition_mismatch",
                    "Prevention requires the installed native method definition.",
                )?;
                let identity = caller_identity(
                    context.caller_principal_id,
                    args.get("operationId")
                        .and_then(IvyJson::as_js_string)
                        .cloned(),
                )?;
                state
                    .operations
                    .run(
                        identity,
                        method.clone(),
                        args.required("params")?.clone(),
                        || async { Err(IvyError::internal()) },
                        true,
                    )?
                    .wait()
                    .await
            }
            "invoke" => {
                require(
                    self.bound(&args, &context, true),
                    "native_definition_mismatch",
                    "Dispatch requires its bound native method and original operation identity.",
                )?;
                let identity = caller_identity(context.caller_principal_id, context.operation_id)?;
                state
                    .dispatch
                    .invoke(
                        identity,
                        args.required("method")?.as_js_string().unwrap().clone(),
                        args.required("params")?.clone(),
                    )?
                    .wait()
                    .await
            }
            "interact" => {
                state.dispatch.available()?;
                require(
                    self.bound(&args, &context, false),
                    "native_definition_mismatch",
                    "Interaction requires its bound native method and original identity.",
                )?;
                let identity = caller_identity(context.caller_principal_id, context.operation_id)?;
                let rpc = state.owner.rpc.clone();
                state
                    .interactions
                    .run(
                        identity.caller_principal_id,
                        identity.operation_id,
                        args.required("method")?.as_js_string().unwrap().clone(),
                        args.required("params")?.clone(),
                        move |request| async move {
                            rpc.request(
                                request.method,
                                request.params,
                                RequestOptions {
                                    request_id: Some(request.request_id.into()),
                                    detach_on_timeout: request.detach_on_timeout,
                                    priority: if request.interaction_priority {
                                        Priority::Interaction
                                    } else {
                                        Priority::Background
                                    },
                                    ..Default::default()
                                },
                            )
                            .await
                        },
                    )?
                    .wait()
                    .await
            }
            "interaction" => {
                state
                    .interactions
                    .lookup(
                        &context.caller_principal_id,
                        &property_string(&args, "operationId")?,
                    )?
                    .wait()
                    .await
            }
            "read" => self.read(args, &context).await,
            "operation" => {
                let operation_id = property_string(&args, "operationId")?;
                let identity =
                    caller_identity(context.caller_principal_id, Some(operation_id.clone()))?;
                state.operations.read(&identity).await?.ok_or_else(|| {
                    let epoch = state.operations.journal().lock().unwrap().epoch().cloned();
                    IvyError::new(
                        "not_found",
                        "Native operation was not found for this caller.",
                    )
                    .with_details(IvyJson::object([
                        ("kind", "agent_operation_absent".into()),
                        ("operationId", IvyJson::String(operation_id)),
                        (
                            "serviceNodeId",
                            IvyJson::String(state.owner.service_node_id.clone()),
                        ),
                        ("epoch", epoch.map(IvyJson::String).unwrap_or(IvyJson::Null)),
                    ]))
                })
            }
            "inputs" => state.operations.journal().lock().unwrap().inputs(&args),
            "inputDefinition" => self.input_definition(&args),
            "answer" => {
                require(
                    context.operation_id.as_ref().is_none_or(|outer| {
                        outer.0.is_empty()
                            || args.get("operationId").and_then(IvyJson::as_js_string)
                                == Some(outer)
                    }),
                    "mutation_conflict",
                    "Outer and answer operation IDs must match.",
                )?;
                let identity = caller_identity(
                    context.caller_principal_id,
                    args.get("operationId")
                        .and_then(IvyJson::as_js_string)
                        .cloned(),
                )?;
                state
                    .dispatch
                    .answer(
                        identity,
                        args.required("identity")?.clone(),
                        args.required("reply")?.clone(),
                    )?
                    .wait()
                    .await
            }
            _ => unreachable!("only the concrete handler map calls this function"),
        }
    }

    async fn read(&self, args: IvyJson, context: &InvocationContext) -> IvyResult<IvyJson> {
        let state = &self.0;
        state.dispatch.available()?;
        let method = args
            .required("method")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        require(
            ["thread/read", "thread/turns/list", "thread/items/list"]
                .iter()
                .any(|name| method == &JsString::from(*name)),
            "invalid_arguments",
            "Read-only access cannot dispatch native mutations.",
        )?;
        require(
            args.get("nativeVersion") == state.owner.catalog.get("version"),
            "native_version_mismatch",
            "Read requires the exact installed native version.",
        )?;
        let request_hash = hash_json(&IvyJson::object([
            ("method", IvyJson::String(method.clone())),
            ("params", args.required("params")?.clone()),
        ]))?;
        let request_id = Arc::new(Mutex::new(None));
        let before_send = Arc::new({
            let request_id = request_id.clone();
            move |id| {
                let request_id = request_id.clone();
                Box::pin(async move {
                    *request_id.lock().unwrap() = Some(id);
                    Ok(())
                }) as HookFuture
            }
        });
        let reply = state
            .owner
            .rpc
            .request(
                method.clone(),
                args.required("params")?.clone(),
                RequestOptions {
                    detach_on_timeout: true,
                    priority: Priority::Interaction,
                    before_send: Some(before_send),
                    ..Default::default()
                },
            )
            .await?;
        state.dispatch.available()?;
        let request_id = request_id.lock().unwrap().clone().ok_or_else(|| {
            IvyError::new(
                "native_observation_invalid",
                "Native observation is missing its dispatched request identity.",
            )
        })?;
        let mut observation = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("observationId", uuid::Uuid::new_v4().to_string().into()),
            (
                "callerPrincipalId",
                IvyJson::String(context.caller_principal_id.clone()),
            ),
            (
                "serviceNodeId",
                IvyJson::String(state.owner.service_node_id.clone()),
            ),
            ("epoch", IvyJson::String(state.owner.epoch.clone())),
            (
                "nativeExecutableHash",
                state
                    .owner
                    .catalog
                    .required("nativeExecutableHash")?
                    .clone(),
            ),
            ("catalogHash", state.catalog_hash.clone().into()),
            ("requestId", request_id),
            ("requestHash", request_hash.into()),
            ("observedAt", IvyJson::String((state.clock)())),
        ]);
        for (key, value) in args.as_object().ok_or_else(IvyError::internal)? {
            observation.insert(key.clone(), value.clone());
        }
        observation.insert("reply", reply);
        Ok(observation)
    }

    fn input_definition(&self, args: &IvyJson) -> IvyResult<IvyJson> {
        let state = &self.0;
        state.dispatch.available()?;
        let input = state
            .operations
            .journal()
            .lock()
            .unwrap()
            .input(args.required("identity")?)?;
        let pending = input.as_ref().filter(|input| {
            input.get("identity").and_then(|value| value.get("serviceNodeId")) == Some(&IvyJson::String(state.owner.service_node_id.clone()))
                && input.get("identity").and_then(|value| value.get("epoch")) == Some(&IvyJson::String(state.owner.epoch.clone()))
                && input.get("state") == Some(&"pending".into())
        }).ok_or_else(|| IvyError::new("native_input_expired",
            "Only an input confirmed pending on the current native owner has an answer definition."))?;
        let definition = array(&state.owner.catalog, "serverRequests")
            .iter()
            .find(|value| value.get("method") == pending.get("method"))
            .ok_or_else(|| {
                IvyError::new(
                    "native_method_unavailable",
                    "The installed catalog does not define this native response.",
                )
            })?;
        Ok(IvyJson::object([
            ("identity", pending.required("identity")?.clone()),
            ("method", pending.required("method")?.clone()),
            (
                "nativeVersion",
                state.owner.catalog.required("version")?.clone(),
            ),
            ("catalogHash", state.catalog_hash.clone().into()),
            (
                "responseSchema",
                definition.required("outputSchema")?.clone(),
            ),
        ]))
    }
}
