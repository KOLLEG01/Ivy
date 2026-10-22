//! Collector's provider boundary. Execution and durable control data stay with
//! the engine; socket generations, routed invocation limits and wire hashes are
//! owned by the shared SDK.
use crate::schema;
use ivy_contracts::{IvyError, IvyJson, IvyResult};
use ivy_sdk::{
    rpc::RpcClient,
    service::{
        Dependency, InvocationContext, ServiceClient, ServiceFuture, ServiceOptions, ServiceState,
    },
};
use std::sync::Arc;

/// The engine and definition owner implement this boundary without retaining a
/// socket across reconnects. Accepted local jobs continue when detach is called.
pub trait CollectorBackend: Send + Sync + 'static {
    fn read(&self, input: IvyJson) -> ServiceFuture<IvyJson>;
    fn update(&self, input: IvyJson, context: InvocationContext) -> ServiceFuture<IvyJson>;
    /// Attach this exact RPC owner, resolve the result root, then synchronize
    /// definitions. Reconciliation must finish before readiness is advertised.
    fn reconcile(&self, client: Arc<dyn RpcClient>) -> ServiceFuture<()>;
    fn detach(&self);
    fn state(&self, state: ServiceState);
    fn ready(&self, _generation: u64) -> ServiceFuture<()> {
        Box::pin(async { Ok(()) })
    }
}

pub fn service_options(
    public_base_url: String,
    credential: Dependency<String>,
    identity: IvyJson,
    backend: Arc<dyn CollectorBackend>,
) -> ServiceOptions {
    let reconciliation = backend.clone();
    let mut options = ServiceOptions::new(
        public_base_url,
        credential,
        identity,
        Arc::new(|| Box::pin(async { Ok(schema::registry()) })),
        Arc::new(move |connection| {
            // Allocate once for this generation, and let the backend reuse it
            // for scoped IDs and publication rather than wrapping each request.
            reconciliation.reconcile(Arc::new(connection))
        }),
    );
    let reader = backend.clone();
    options.handlers.insert(
        "data-collector.read".into(),
        Arc::new(move |input, _| {
            let reader = reader.clone();
            Box::pin(async move {
                reader
                    .read(schema::parse_schema("readSchema", &input)?)
                    .await
            })
        }),
    );
    let writer = backend.clone();
    options.handlers.insert(
        "data-collector.update".into(),
        Arc::new(move |input, context| {
            let writer = writer.clone();
            Box::pin(async move {
                let input = schema::parse_schema("updateSchema", &input)?;
                if context.operation_id.as_ref().is_some_and(|id| {
                    !id.0.is_empty()
                        && input.get("operationId").and_then(IvyJson::as_js_string) != Some(id)
                }) {
                    return Err(IvyError::new(
                        "operation_conflict",
                        "Routed and inner operation IDs must match.",
                    ));
                }
                writer.update(input, context).await
            })
        }),
    );
    let ready = backend.clone();
    options.readiness = Some(Arc::new(move |connection| {
        let ready = ready.clone();
        Box::pin(async move {
            ready.ready(connection.generation()).await?;
            Ok(IvyJson::object([
                ("ready", true.into()),
                ("diagnostics", IvyJson::Array(Vec::new())),
            ]))
        })
    }));
    options.heartbeat = std::time::Duration::from_secs(2);
    options.on_state = Some(Arc::new(move |state| {
        if matches!(state.status, "connecting" | "offline" | "stopped") {
            backend.detach();
        }
        backend.state(state);
    }));
    options
}

pub fn service(
    public_base_url: String,
    credential: Dependency<String>,
    identity: IvyJson,
    backend: Arc<dyn CollectorBackend>,
) -> IvyResult<ServiceClient> {
    ServiceClient::new(service_options(
        public_base_url,
        credential,
        identity,
        backend,
    ))
}
