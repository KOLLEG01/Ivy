//! Raw HTTP/1 transport shared only by the Collector's two listeners.
use crate::http_compat::NodeIo;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    json::{parse, stringify},
};
use ivy_sdk::service::ServiceFuture;
use std::{convert::Infallible, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{io::AsyncWriteExt, net::TcpListener, sync::watch, task::JoinSet};
use tokio_util::sync::CancellationToken;

pub type Reply = Response<Full<Bytes>>;
pub type Handler = Arc<dyn Fn(Request<Incoming>) -> ServiceFuture<Reply> + Send + Sync>;

struct Inner {
    stop: CancellationToken,
    done: watch::Receiver<bool>,
    address: SocketAddr,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
#[derive(Clone)]
pub struct Server(Arc<Inner>);
impl Server {
    pub async fn bind(host: &str, port: u16, handler: Handler) -> IvyResult<Self> {
        let listener = TcpListener::bind((host, port))
            .await
            .map_err(|_| IvyError::internal())?;
        let address = listener.local_addr().map_err(|_| IvyError::internal())?;
        let stop = CancellationToken::new();
        let cancellation = stop.clone();
        let (finished, done) = watch::channel(false);
        tokio::spawn(async move {
            let mut connections = JoinSet::new();
            let mut scan = tokio::time::interval_at(
                tokio::time::Instant::now() + Duration::from_secs(30),
                Duration::from_secs(30),
            );
            scan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let (checked, _) = watch::channel(0u64);
            let mut generation = 0;
            loop {
                tokio::select! { biased;
                    _=cancellation.cancelled()=>break,
                    Some(_)=connections.join_next()=>{},
                    _=scan.tick()=>{generation+=1;checked.send_replace(generation);},
                    accepted=listener.accept()=>{
                        let Ok((socket,_))=accepted else { break; };
                        let handler=handler.clone();
                        let mut checks=checked.subscribe();
                        connections.spawn(async move {
                            let io=NodeIo::new(socket);
                            let capture=io.capture.clone();
                            let observed=capture.clone();
                            let changed=capture.lock().unwrap().changed.clone();
                            let service=service_fn(move |mut request:Request<Incoming>| {
                                let handler=handler.clone();
                                let headers=observed.lock().unwrap().view();
                                async move {
                                    let Some(headers)=headers else {return Ok::<_,Infallible>(close_response(empty(400)));};
                                    *request.headers_mut()=headers;
                                    if request.version()==hyper::Version::HTTP_11 && !request.headers().contains_key(hyper::header::HOST) {return Ok(close_response(empty(400)));}
                                    Ok(handler(request).await.unwrap_or_else(|_|empty(400)))
                                }
                            });
                            let mut builder=http1::Builder::new();
                            builder.header_read_timeout(None).max_headers(1004).max_buf_size(32_768);
                            let mut connection=builder.serve_connection(TokioIo::new(io),service);
                            let status=loop {
                                tokio::select! {
                                    _=&mut connection=>return,
                                    _=changed.notified()=>{if let Some(status)=capture.lock().unwrap().rejection() {break Some(status);}},
                                    result=checks.changed()=>{
                                        if result.is_err() {return;}
                                        let state=capture.lock().unwrap();
                                        if state.expired() {break state.can_report_timeout().then_some(408);}
                                    },
                                }
                            };
                            let mut socket=connection.into_parts().io.into_inner().socket;
                            if let Some(status)=status {
                                let reason=match status {408=>"Request Timeout",431=>"Request Header Fields Too Large",_=>"Bad Request"};
                                let reply=format!("HTTP/1.1 {status} {reason}\r\nConnection: close\r\n\r\n");
                                let _=tokio::time::timeout(Duration::from_secs(1),socket.write_all(reply.as_bytes())).await;
                            }
                            let _=socket.shutdown().await;
                        });
                    }
                }
            }
            drop(listener);
            connections.abort_all();
            while connections.join_next().await.is_some() {}
            let _ = finished.send(true);
        });
        Ok(Self(Arc::new(Inner {
            stop,
            done,
            address,
        })))
    }
    pub fn address(&self) -> SocketAddr {
        self.0.address
    }
    pub async fn close(&self) {
        self.0.stop.cancel();
        let mut done = self.0.done.clone();
        while !*done.borrow_and_update() {
            if done.changed().await.is_err() {
                break;
            }
        }
    }
}
pub fn empty(status: u16) -> Reply {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::new()))
        .unwrap()
}
pub fn encoded(status: u16, body: String) -> Reply {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap()
}
pub fn json(status: u16, body: IvyJson) -> IvyResult<Reply> {
    Ok(encoded(status, stringify(&body, usize::MAX)?))
}
pub fn close_response(mut response: Reply) -> Reply {
    response.headers_mut().insert(
        "Connection",
        hyper::header::HeaderValue::from_static("close"),
    );
    response
}

// Node's IncomingMessage decodes header bytes as latin1. Authorization uses the
// first duplicate; ordinary fields such as idempotency-key join with comma-space.
pub fn header(headers: &hyper::HeaderMap, name: &str, join: bool) -> Option<String> {
    let values: Vec<String> = headers
        .get_all(name)
        .iter()
        .map(|h| h.as_bytes().iter().map(|b| char::from(*b)).collect())
        .collect();
    if join {
        (!values.is_empty()).then(|| values.join(", "))
    } else {
        values.into_iter().next()
    }
}
pub fn bearer(headers: &hyper::HeaderMap) -> String {
    let text = header(headers, "authorization", false).unwrap_or_default();
    text.strip_prefix("Bearer ").unwrap_or(&text).to_owned()
}
pub fn equal(left: &[u8], right: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    left.len() == right.len() && bool::from(left.ct_eq(right))
}
pub enum BodyError {
    TooLarge,
    Failed,
}
pub async fn body(mut incoming: Incoming, limit: usize) -> Result<IvyJson, BodyError> {
    let mut bytes = Vec::new();
    while let Some(frame) = incoming.frame().await {
        let frame = frame.map_err(|_| BodyError::Failed)?;
        if let Ok(chunk) = frame.into_data() {
            if chunk.len() > limit.saturating_sub(bytes.len()) {
                return Err(BodyError::TooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
    }
    // Buffer.toString('utf8') replaces invalid byte sequences before JSON.parse.
    parse(&String::from_utf8_lossy(&bytes), usize::MAX).map_err(|_| BodyError::Failed)
}
pub fn drain(mut body: Incoming) {
    tokio::spawn(async move {
        while let Some(frame) = body.frame().await {
            if frame.is_err() {
                break;
            }
        }
    });
}
