//! Import-only Hive write admission through the journal's existing local owner.
//! The Send RPC boundary holds a bounded channel, never a SQLite reference.
use crate::{import_state, journal::Journal};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, hash::hash_json, json::canonical,
    operation::parse_operation_id_js,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
pub(crate) struct Prepared {
    pub key: String,
    pub params: IvyJson,
    pub response: Option<IvyJson>,
}
pub(crate) enum Admission {
    Prepare {
        params: IvyJson,
        reply: oneshot::Sender<IvyResult<Prepared>>,
    },
    Complete {
        key: String,
        response: IvyJson,
        reply: oneshot::Sender<IvyResult<()>>,
    },
}
struct ImportRpc {
    inner: Arc<dyn RpcClient>,
    admissions: mpsc::Sender<Admission>,
}
fn closed() -> IvyError {
    IvyError::new(
        "secretary_import_closed",
        "The import journal owner is unavailable.",
    )
}
impl RpcClient for ImportRpc {
    fn service_connection(&self) -> Option<&ivy_sdk::service::ServiceConnection> {
        self.inner.service_connection()
    }
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if method != "objects.write" {
                return self.inner.request(method, params, options).await;
            }
            let (reply, receive) = oneshot::channel();
            self.admissions
                .send(Admission::Prepare { params, reply })
                .await
                .map_err(|_| closed())?;
            let prepared = receive.await.map_err(|_| closed())??;
            if let Some(response) = prepared.response {
                return Ok(response);
            }
            let response = self.inner.request(method, prepared.params, options).await?;
            let (reply, receive) = oneshot::channel();
            self.admissions
                .send(Admission::Complete {
                    key: prepared.key,
                    response: response.clone(),
                    reply,
                })
                .await
                .map_err(|_| closed())?;
            receive.await.map_err(|_| closed())??;
            Ok(response)
        })
    }
}
pub(crate) fn client(inner: Arc<dyn RpcClient>) -> (Arc<dyn RpcClient>, mpsc::Receiver<Admission>) {
    let (admissions, receiver) = mpsc::channel(1);
    (Arc::new(ImportRpc { inner, admissions }), receiver)
}
pub(crate) fn prefix(state: &IvyJson) -> IvyResult<String> {
    Ok(format!(
        "secretary/import/write/{}/",
        &hash_json(state.required("operationId")?)?[7..]
    ))
}
fn without_mutation(params: &IvyJson) -> IvyJson {
    let mut value = params.clone();
    value.remove("mutationId");
    value
}
pub(crate) fn admit(journal: &Journal, state: &IvyJson, admission: Admission) {
    match admission {
        Admission::Prepare { params, reply } => {
            let result = (|| {
                let key = format!(
                    "{}{}",
                    prefix(state)?,
                    &hash_json(&without_mutation(&params))?[7..]
                );
                let saved = if let Some(saved) = import_state::read(journal, &key)? {
                    saved
                } else {
                    let value =
                        IvyJson::object([("params", params.clone()), ("response", IvyJson::Null)]);
                    import_state::save(journal, &key, &value)?;
                    value
                };
                let original = saved.required("params")?;
                if canonical(&without_mutation(original), 16 * 1024 * 1024)?
                    != canonical(&without_mutation(&params), 16 * 1024 * 1024)?
                {
                    return Err(IvyError::new(
                        "secretary_import_identity_conflict",
                        "The original import write arguments changed.",
                    ));
                }
                parse_operation_id_js(
                    original
                        .required("mutationId")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?,
                )?;
                Ok(Prepared {
                    key,
                    params: original.clone(),
                    response: saved.get("response").filter(|v| !v.is_null()).cloned(),
                })
            })();
            let _ = reply.send(result);
        }
        Admission::Complete {
            key,
            response,
            reply,
        } => {
            let result = (|| {
                if !key.starts_with(&prefix(state)?) {
                    return Err(IvyError::internal());
                }
                let mut saved =
                    import_state::read(journal, &key)?.ok_or_else(IvyError::internal)?;
                saved.insert("response", response);
                import_state::save(journal, &key, &saved)
            })();
            let _ = reply.send(result);
        }
    }
}
