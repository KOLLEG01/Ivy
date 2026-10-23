//! Browser-push delivery through the native HTTP client.
use crate::browser_push_native::{Https, PostFuture, Request, SOCKET_TIMEOUT};
use ivy_contracts::{IvyError, IvyResult};

#[derive(Clone)]
pub struct NativeHttps(reqwest::Client);
impl NativeHttps {
    pub fn new() -> IvyResult<Self> {
        Self::from_builder(reqwest::Client::builder())
    }
    /// The caller may supply trust roots or a resolver. Delivery always uses
    /// HTTPS, one HTTP/1 request, no proxy or redirect, and a bounded deadline.
    pub fn from_builder(builder: reqwest::ClientBuilder) -> IvyResult<Self> {
        builder
            .https_only(true)
            .http1_only()
            .no_proxy()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(SOCKET_TIMEOUT)
            .build()
            .map(Self)
            .map_err(|_| IvyError::internal())
    }
}
impl Https for NativeHttps {
    fn post(&self, request: Request) -> PostFuture {
        let client = self.0.clone();
        Box::pin(async move {
            let mut builder = client.post(request.endpoint);
            for (name, value) in request.headers {
                builder = builder.header(name, value);
            }
            let mut response = builder
                .body(request.body)
                .send()
                .await
                .map_err(|_| IvyError::internal())?;
            let status = response.status().as_u16();
            while response
                .chunk()
                .await
                .map_err(|_| IvyError::internal())?
                .is_some()
            {}
            Ok(status)
        })
    }
}
