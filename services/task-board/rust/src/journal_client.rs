use ivy_sdk::{
    IvyJson, IvyResult,
    admission::{RequestPermit, RequestQueue},
    rpc::{OptionalRpcClient, RequestFuture, RequestOptions, RpcClient},
    service::ServiceConnection,
};
use std::sync::Arc;

/// One admission queue is shared by every remote consumer of a journal generation.
pub struct JournalClient<R> {
    connection: R,
    requests: Arc<RequestQueue>,
}
impl<R: RpcClient> JournalClient<R> {
    pub fn new(connection: R) -> IvyResult<Self> {
        Ok(Self {
            connection,
            requests: RequestQueue::new(48, 16, 512)?,
        })
    }
    async fn admit(&self, method: &str, options: &RequestOptions) -> IvyResult<RequestPermit> {
        self.requests
            .acquire(
                matches!(method, "tools.call" | "discovery.call"),
                &options.cancellation,
            )
            .await
            .map_err(|error| {
                if error.code == "cancelled" {
                    options.cancel_error.clone()
                } else {
                    error
                }
            })
    }
}
impl<R: RpcClient> RpcClient for JournalClient<R> {
    fn service_connection(&self) -> Option<&ServiceConnection> {
        self.connection.service_connection()
    }
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let _permit = self.admit(method, &options).await?;
            self.connection.request(method, params, options).await
        })
    }
}
impl<R: OptionalRpcClient> OptionalRpcClient for JournalClient<R> {
    fn request_optional<'a>(
        &'a self,
        method: &'a str,
        params: Option<IvyJson>,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let _permit = self.admit(method, &options).await?;
            self.connection
                .request_optional(method, params, options)
                .await
        })
    }
}
