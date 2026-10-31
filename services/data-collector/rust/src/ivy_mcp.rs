//! Explicit run-local Ivy bindings. Scripts never receive a Hive connection or credential.
use crate::{mcp_bridge::TaskMcp, mcp_context::text, model};
use ivy_contracts::{IvyError, IvyJson, JsString};
use ivy_sdk::{
    rpc::{RequestOptions, RpcClient},
    service::ServiceFuture,
    tools::{InterfaceRequirement, ToolCallOptions, ToolClients},
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) struct IvyMcp {
    client: Arc<dyn RpcClient>,
    tools: Arc<ToolClients>,
    configuration: IvyJson,
    signal: CancellationToken,
}
fn denied() -> IvyError {
    IvyError::new(
        "configuration_invalid",
        "The call does not match an explicit Ivy binding.",
    )
}
impl IvyMcp {
    pub fn new(
        client: Arc<dyn RpcClient>,
        tools: Arc<ToolClients>,
        configuration: IvyJson,
        signal: CancellationToken,
    ) -> Self {
        Self {
            client,
            tools,
            configuration,
            signal,
        }
    }
}
impl TaskMcp for IvyMcp {
    fn call(&self, tool: JsString, args: IvyJson) -> ServiceFuture<IvyJson> {
        let client = self.client.clone();
        let tools = self.tools.clone();
        let configuration = self.configuration.clone();
        let signal = self.signal.clone();
        Box::pin(async move {
            let bindings = configuration
                .required("bindings")?
                .as_array()
                .ok_or_else(denied)?;
            let candidates: Vec<_> = bindings
                .iter()
                .filter(|b| {
                    b.get("tool").and_then(IvyJson::as_js_string) == Some(&tool)
                        && (b.get("kind") == Some(&"core".into())
                            || b.get("serviceNodeId") == args.get("serviceNodeId"))
                })
                .collect();
            if candidates.len() != 1 {
                return Err(denied());
            }
            let binding = candidates[0];
            let options = RequestOptions {
                cancellation: signal,
                ..Default::default()
            };
            if options.cancellation.is_cancelled() {
                return Err(options.cancel_error.clone());
            }
            // Bind the authenticated collector principal explicitly. Migrating a definition
            // cannot silently inherit the former AgentManager's broader routing rights.
            let status = client
                .request("system.status", model::object(), options.clone())
                .await?;
            let caller = text(&configuration, "callerPrincipalId")?;
            if status
                .get("callerPrincipalId")
                .and_then(IvyJson::as_js_string)
                != Some(&caller)
            {
                return Err(IvyError::new(
                    "target_conflict",
                    "The configured Ivy caller has changed.",
                ));
            }
            let value = if binding.get("kind") == Some(&"core".into()) {
                if tool != JsString::from("hive_status")
                    || !args.as_object().is_some_and(|a| a.is_empty())
                {
                    return Err(IvyError::new(
                        "invalid_arguments",
                        "Hive status accepts only an empty argument object.",
                    ));
                }
                // This is the only core operation granted by this adapter. Preserve the
                // actual authenticated system.status value, including runtimeEpoch.
                status
            } else {
                let entries = args.as_object().ok_or_else(denied)?;
                if entries.iter().any(|(k, _)| {
                    !["serviceNodeId", "input", "operationId"]
                        .iter()
                        .any(|s| k == &JsString::from(*s))
                }) {
                    return Err(IvyError::new(
                        "invalid_arguments",
                        "Unexpected routed tool arguments.",
                    ));
                }
                let input = args.required("input")?.clone();
                let operation = args
                    .get("operationId")
                    .map(|_| text(&args, "operationId"))
                    .transpose()?;
                if operation.as_ref().is_some_and(|id| {
                    input
                        .get("operationId")
                        .is_some_and(|inner| inner.as_js_string() != Some(id))
                }) {
                    return Err(IvyError::new(
                        "operation_conflict",
                        "Routed and inner operation IDs must match.",
                    ));
                }
                let name = text(binding, "qualifiedName")?;
                let qualified = name.to_utf8()?;
                let (namespace, _) = qualified.split_once('.').ok_or_else(denied)?;
                let selected = tools.service_tools(
                    text(binding, "serviceNodeId")?,
                    &[InterfaceRequirement {
                        namespace: namespace.to_owned(),
                        interface_version: text(binding, "interfaceVersion")?.to_utf8()?,
                    }],
                )?;
                let resolved = tokio::select! { biased;
                    _ = options.cancellation.cancelled() => return Err(options.cancel_error.clone()),
                    result = selected.binding(&name, None) => result?,
                };
                if resolved
                    .value()
                    .get("definition")
                    .and_then(|v| v.get("annotations"))
                    .and_then(|v| v.get("readOnlyHint"))
                    != Some(&true.into())
                    && operation.as_ref().is_none_or(|id| id.0.is_empty())
                {
                    return Err(IvyError::new(
                        "invalid_arguments",
                        "A service mutation requires its original operation ID.",
                    ));
                }
                selected
                    .call(
                        &name,
                        input,
                        operation,
                        ToolCallOptions {
                            request: options,
                            expected_caller_principal_id: Some(caller),
                        },
                    )
                    .await?
            };
            Ok(IvyJson::object([("result", value)]))
        })
    }
    fn close(&self) -> ServiceFuture<()> {
        self.signal.cancel();
        Box::pin(async { Ok(()) })
    }
}
