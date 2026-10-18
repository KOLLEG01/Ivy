//! Phone Voice call policy over the common native transport. The connection owner
//! supplies catalog schema validation; neither process launch nor journal ownership
//! moves into this adapter.
use ivy_contracts::{IvyError, IvyJson, IvyResult};
use ivy_host_runtime::codex_rpc::{Events, NativeRpc, RequestOptions};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

/// Required compiler boundary for installed native schemas. Implementations must
/// apply the catalog schema and the supplied serialized-value byte limit.
pub trait VoiceSchemas: Send + Sync {
    fn validate(&self, schema: &IvyJson, value: &IvyJson, max_bytes: usize) -> IvyResult<()>;
}

#[derive(Clone)]
pub struct VoiceRpc {
    rpc: NativeRpc,
    catalog: Arc<IvyJson>,
    schemas: Arc<dyn VoiceSchemas>,
}

pub fn call_timeout_ms(method: &str) -> u64 {
    if method == "model/list" {
        15_000
    } else if method.starts_with("thread/realtime/") {
        30_000
    } else {
        120_000
    }
}

impl VoiceRpc {
    pub fn start<W, R>(
        catalog: IvyJson,
        input: W,
        output: R,
        events: Events,
        schemas: Arc<dyn VoiceSchemas>,
    ) -> IvyResult<Self>
    where
        W: AsyncWrite + Unpin + Send + 'static,
        R: AsyncRead + Unpin + Send + 'static,
    {
        let rpc = NativeRpc::start(&catalog, input, output, events)?;
        Ok(Self {
            rpc,
            catalog: Arc::new(catalog),
            schemas,
        })
    }

    pub fn transport(&self) -> &NativeRpc {
        &self.rpc
    }

    /// Original IDs and durable hooks pass unchanged to the shared actor. A dropped
    /// caller does not cancel its dispatched request; explicit timeout detachment
    /// follows the native actor's separate observation semantics.
    pub async fn call(
        &self,
        method: &str,
        params: IvyJson,
        mut hooks: RequestOptions,
        timeout_ms: Option<u64>,
    ) -> IvyResult<IvyJson> {
        if !self.rpc.connected() {
            return Err(IvyError::new(
                "phone_voice_unavailable",
                "Codex Voice transport is unavailable.",
            ));
        }
        let definition = self
            .catalog
            .get("clientRequests")
            .and_then(IvyJson::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item.get("method") == Some(&IvyJson::from(method)))
            })
            .ok_or_else(|| {
                IvyError::new(
                    "native_method_unavailable",
                    "Configured Codex does not support the requested Voice operation.",
                )
            })?;
        self.schemas.validate(
            definition
                .get("inputSchema")
                .ok_or_else(IvyError::internal)?,
            &params,
            6 * 1024 * 1024,
        )?;
        hooks.timeout_ms = timeout_ms.unwrap_or_else(|| call_timeout_ms(method));
        let reply = self.rpc.request(method.into(), params, hooks).await?;
        if let Some(error) = reply.get("error") {
            return Err(IvyError::new(
                "phone_codex_request_failed",
                "Codex rejected the Voice operation.",
            )
            .with_details(error.clone()));
        }
        // The common transport validates the reply envelope. Output schema checks
        // remain with the owning operation, as in the original Voice caller.
        reply.get("result").cloned().ok_or_else(IvyError::internal)
    }

    /// Maintenance expiry must leave a subsequent active call connected.
    pub async fn archive_call(
        &self,
        method: &str,
        params: IvyJson,
        mut hooks: RequestOptions,
    ) -> IvyResult<IvyJson> {
        hooks.detach_on_timeout = true;
        self.call(method, params, hooks, Some(15_000)).await
    }
}
