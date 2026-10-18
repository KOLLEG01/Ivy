//! Accepted-cache HTTP inputs. The final authentication check and admission run
//! together on the store owner, including revision changes while reading a body.
use crate::{
    engine::Engine,
    http::{self, BodyError, Reply, Server},
    model,
    runtime::Runtime,
};
use hyper::{Request, body::Incoming};
use ivy_contracts::{IvyError, IvyJson, IvyResult};
use std::{net::SocketAddr, sync::Arc};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Ingress {
    server: Server,
    closed: CancellationToken,
}
impl Ingress {
    pub async fn open(engine: Engine) -> IvyResult<Option<Self>> {
        let Some(config) = engine.settings().get("ingress").filter(|c| !c.is_null()) else {
            return Ok(None);
        };
        let host = model::text(config, "host")?;
        let port = model::number(config, "port")? as u16;
        let closed = CancellationToken::new();
        let stopped = closed.clone();
        let server = Server::bind(
            &host,
            port,
            Arc::new(move |request| {
                let engine = engine.clone();
                let closed = stopped.clone();
                Box::pin(async move {
                    match handle(engine, closed, request).await {
                        Ok(reply) => Ok(reply),
                        Err(error) => http::json(
                            if error.code == "task_busy" { 409 } else { 400 },
                            IvyJson::object([("error", IvyJson::String(error.code_js()))]),
                        ),
                    }
                })
            }),
        )
        .await?;
        Ok(Some(Self { server, closed }))
    }
    pub fn address(&self) -> SocketAddr {
        self.server.address()
    }
    pub(crate) fn begin_close(&self) {
        self.closed.cancel();
    }
    pub async fn close(&self) {
        self.begin_close();
        self.server.close().await;
    }
}
fn authorized(row: Option<&IvyJson>, settings: &IvyJson, supplied: &str) -> bool {
    let Some(task) = row.and_then(|r| r.get("task")) else {
        return false;
    };
    if !model::truth(task, "enabled") {
        return false;
    }
    if model::truth(task, "allowUnauthenticatedInput") {
        return true;
    }
    let expected = task
        .get("inputSecretName")
        .and_then(|n| n.as_js_string())
        .and_then(|n| {
            settings
                .get("secrets")
                .and_then(|s| s.as_object())
                .and_then(|pairs| pairs.iter().find(|(key, _)| key == n))
                .and_then(|(_, v)| v.as_js_string())
        });
    expected.is_some_and(|value| {
        !value.0.is_empty()
            && http::equal(
                String::from_utf16_lossy(&value.0).as_bytes(),
                supplied.as_bytes(),
            )
    })
}
fn route(path: &str, settings: &IvyJson) -> Option<String> {
    if let Some(id) = path
        .strip_prefix("/tasks/")
        .and_then(|v| v.strip_suffix("/run"))
    {
        let bytes = id.as_bytes();
        if (1..=64).contains(&bytes.len())
            && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
            && bytes
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
        {
            return Some(id.to_owned());
        }
    }
    settings
        .get("ingress")?
        .get("routes")?
        .get(path)?
        .as_str()
        .ok()
}
async fn handle(
    engine: Engine,
    closed: CancellationToken,
    request: Request<Incoming>,
) -> IvyResult<Reply> {
    let id = route(&request.uri().to_string(), engine.settings());
    let supplied = http::bearer(request.headers());
    let operation = http::header(request.headers(), "idempotency-key", true)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let initial_id = id.clone();
    let settings = engine.settings().clone();
    let token = supplied.clone();
    let row = engine
        .store()
        .call(move |store| {
            let row = initial_id
                .map(|id| store.get(&id, false))
                .transpose()?
                .flatten();
            Ok(if authorized(row.as_ref(), &settings, &token) {
                row
            } else {
                None
            })
        })
        .await?;
    if request.method() != "POST" || row.is_none() {
        http::drain(request.into_body());
        return Ok(http::empty(401));
    }
    if closed.is_cancelled() {
        http::drain(request.into_body());
        return Ok(http::empty(503));
    }
    let input = match http::body(request.into_body(), crate::policy::INPUT_BYTES).await {
        Ok(input) => input,
        Err(BodyError::TooLarge) => return Ok(http::close_response(http::empty(413))),
        Err(BodyError::Timeout) => return Ok(http::close_response(http::empty(408))),
        Err(BodyError::Failed) => return Err(IvyError::internal()),
    };
    let revision = row.unwrap().get("revision").cloned();
    let id = id.unwrap();
    let this = engine.clone();
    let now = engine.now();
    let date = engine.now_iso()?;
    engine
        .store()
        .call(move |store| {
            let current = store.get(&id, false)?;
            if !authorized(current.as_ref(), this.settings(), &supplied)
                || current.as_ref().and_then(|r| r.get("revision")) != revision.as_ref()
            {
                return Ok(http::empty(401));
            }
            if closed.is_cancelled() {
                return Ok(http::empty(503));
            }
            let result = Runtime {
                store,
                settings: this.settings(),
                root_id: &this.root_id(),
            }
            .update(
                &IvyJson::object([
                    ("action", "run".into()),
                    ("id", id.into()),
                    ("operationId", operation.into()),
                    ("input", input),
                ]),
                None,
                now,
                &date,
                |id| this.active(id),
                |id| this.runner().clear_authentication(id),
            )?;
            http::json(202, result.value)
        })
        .await
}
