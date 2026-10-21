//! Server invocation admission and JSON-RPC batches over the storage owner.
//! The network executor supplies its direct dispatcher and authenticated context.
use crate::{
    admission::{DispatchGate, RequestBudget},
    rpc,
    worker_client::RequestOptions,
    worker_runtime::StorageClient,
    ws_send::encode,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    limits::{MANAGEMENT_FRAME_BYTES, RPC_BATCH_REQUEST_BYTES, RPC_BATCH_REQUESTS},
    validation::validate_request,
};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;

pub type Reply<T = IvyJson> = Pin<Box<dyn Future<Output = IvyResult<T>> + Send>>;
/// Posting is eager, including its reservation. The returned future only observes
/// the posted work; abandoning it must not cancel the storage request.
pub trait Storage: Send + Sync {
    fn request(&self, payload: IvyJson, options: RequestOptions) -> Reply;
}
impl Storage for StorageClient {
    fn request(&self, payload: IvyJson, options: RequestOptions) -> Reply {
        let reply = StorageClient::request(self, payload, options);
        Box::pin(async move { reply.await.map_err(|_| IvyError::internal())? })
    }
}
/// Direct routing starts eagerly and may throw before returning its promise.
/// Freshness is checked by Invocation::direct before this callback is entered.
pub type Direct = Arc<dyn Fn(&IvyJson, &IvyJson) -> IvyResult<Option<Reply>> + Send + Sync>;
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

#[derive(Default)]
pub struct Options {
    pub reset_bootstrap_credential_digest: Option<JsString>,
}
struct Parts {
    storage: Arc<dyn Storage>,
    direct: Direct,
    now: Clock,
    requests: RequestBudget,
    dispatch: DispatchGate,
    options: Options,
}
/// Invoke and the socket dispatcher share these exact admission owners. Accepted
/// work runs on the network executor even when its caller drops the result.
#[derive(Clone)]
pub struct Invocation(Arc<Parts>);

pub fn require_fresh(context: &IvyJson, now: &dyn Fn() -> f64) -> IvyResult<()> {
    if let Some(expires_at) = context.get("expiresAt") {
        let now = now();
        if !expires_at.as_f64().is_some_and(|expires| expires > now) {
            return Err(IvyError::new(
                "unauthenticated",
                "OAuth access token has expired.",
            ));
        }
    }
    Ok(())
}
fn ready<T: Send + 'static>(value: IvyResult<T>) -> Reply<T> {
    Box::pin(std::future::ready(value))
}
fn receive<T: Send + 'static>(reply: oneshot::Receiver<IvyResult<T>>) -> Reply<T> {
    Box::pin(async move { reply.await.map_err(|_| IvyError::internal())? })
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}

impl Invocation {
    pub fn new(
        storage: Arc<dyn Storage>,
        direct: Direct,
        now: Clock,
        requests: RequestBudget,
        dispatch: DispatchGate,
        options: Options,
    ) -> Self {
        Self(Arc::new(Parts {
            storage,
            direct,
            now,
            requests,
            dispatch,
            options,
        }))
    }
    pub fn direct(&self, context: &IvyJson, request: &IvyJson) -> IvyResult<Option<Reply>> {
        require_fresh(context, &*self.0.now)?;
        (self.0.direct)(context, request)
    }
    pub fn invoke(&self, context: IvyJson, request: IvyJson, received_bytes: f64) -> Reply {
        if let Err(error) = require_fresh(&context, &*self.0.now) {
            return ready(Err(error));
        }
        let request_bytes = if received_bytes > 0.0 {
            received_bytes
        } else {
            4096.0
        };
        let digest = match context
            .get("credentialDigest")
            .and_then(IvyJson::as_js_string)
        {
            Some(value) => value,
            None => return ready(Err(IvyError::internal())),
        };
        let mut lease = match self
            .0
            .requests
            .acquire(digest, Some(1024.0 + request_bytes))
        {
            Ok(lease) => lease,
            Err(error) => return ready(Err(error)),
        };
        let work = (|| -> IvyResult<Reply> {
            if let Some(digest) = &self.0.options.reset_bootstrap_credential_digest {
                if !digest.0.is_empty() {
                    require(
                        context.get("transport") == Some(&"http".into())
                            && context.get("credentialDigest")
                                == Some(&IvyJson::String(digest.clone())),
                        "maintenance_active",
                        "Hive accepts only the configured ui bootstrap while the runtime reset is held.",
                    )?;
                }
            }
            require_fresh(&context, &*self.0.now)?;
            if let Some(direct) = self.direct(&context, &request)? {
                return Ok(direct);
            }
            let this = self.clone();
            Ok(receive(self.0.dispatch.run(move || async move {
                require_fresh(&context, &*this.0.now)?;
                let prepared = this
                    .0
                    .storage
                    .request(
                        IvyJson::object([
                            ("action", "execute".into()),
                            ("context", context),
                            ("request", request),
                        ]),
                        RequestOptions {
                            reserved_bytes: request_bytes,
                            ..Default::default()
                        },
                    )
                    .await?;
                prepared
                    .get("value")
                    .cloned()
                    .ok_or_else(IvyError::internal)
            })))
        })();
        match work {
            Err(error) => {
                lease.release();
                ready(Err(error))
            }
            Ok(work) => {
                let (result, reply) = oneshot::channel();
                tokio::spawn(async move {
                    let value = work.await;
                    lease.release();
                    let _ = result.send(value);
                });
                receive(reply)
            }
        }
    }
    /// Each item retains normal invocation admission. Replies stay in input
    /// order, while successfully encoded items consume the shared byte budget
    /// in completion order. The 4096-byte reserve covers each fallback reply.
    pub fn batch(
        &self,
        context: IvyJson,
        frames: Vec<IvyJson>,
        received_bytes: f64,
    ) -> Reply<String> {
        let ids = (|| -> IvyResult<Vec<IvyJson>> {
            require(
                !frames.is_empty(),
                "invalid_frame",
                "Expected a nonempty RPC batch.",
            )?;
            require(
                frames.len() <= RPC_BATCH_REQUESTS
                    && received_bytes <= RPC_BATCH_REQUEST_BYTES as f64,
                "limit_exceeded",
                "RPC batch exceeds its request count or byte limit.",
            )?;
            let ids: Vec<_> = frames
                .iter()
                .map(|frame| rpc::valid_request_id(frame.get("id")))
                .collect();
            require(
                ids.iter()
                    .enumerate()
                    .all(|(index, id)| id.is_null() || !ids[..index].contains(id)),
                "invalid_frame",
                "RPC batch request identities must be distinct.",
            )?;
            Ok(ids)
        })();
        let ids = match ids {
            Ok(ids) => ids,
            Err(error) => return ready(Err(error)),
        };
        let remaining = Arc::new(Mutex::new(
            MANAGEMENT_FRAME_BYTES - 2 - (frames.len() - 1) - frames.len() * 4096,
        ));
        let mut replies = Vec::with_capacity(frames.len());
        for (frame, id) in frames.into_iter().zip(ids) {
            let prepared = validate_request(&frame).and_then(|()| Ok(encode(&frame)?.len()));
            let work = match prepared {
                // There is no await on the original validation-error path. Its
                // encoding consumes budget before the next item starts.
                Err(error) => ready(batch_reply(
                    rpc::failure(&error, id.clone()),
                    id,
                    &remaining,
                )),
                Ok(bytes) => {
                    let work = self.invoke(context.clone(), frame, bytes as f64);
                    let remaining = remaining.clone();
                    let (result, reply) = oneshot::channel();
                    tokio::spawn(async move {
                        let reply = match work.await {
                            Ok(value) => IvyJson::object([
                                ("jsonrpc", "2.0".into()),
                                ("id", id.clone()),
                                ("result", value),
                            ]),
                            Err(error) => rpc::failure(&error, id.clone()),
                        };
                        let _ = result.send(batch_reply(reply, id, &remaining));
                    });
                    receive(reply)
                }
            };
            replies.push(work);
        }
        let (result, reply) = oneshot::channel();
        tokio::spawn(async move {
            let work = async {
                let mut body = String::from("[");
                for (index, reply) in replies.into_iter().enumerate() {
                    if index > 0 {
                        body.push(',');
                    }
                    body.push_str(&reply.await?);
                }
                body.push(']');
                Ok(body)
            }
            .await;
            let _ = result.send(work);
        });
        receive(reply)
    }
}
fn batch_reply(reply: IvyJson, id: IvyJson, remaining: &Mutex<usize>) -> IvyResult<String> {
    if let Ok(encoded) = encode(&reply) {
        let mut remaining = remaining.lock().unwrap();
        if encoded.len() <= *remaining {
            *remaining -= encoded.len();
            return Ok(encoded);
        }
    }
    encode(&rpc::failure(
        &IvyError::new(
            "result_too_large",
            "RPC batch item cannot fit its response frame.",
        )
        .with_outcome(Outcome::Unknown),
        id,
    ))
}
