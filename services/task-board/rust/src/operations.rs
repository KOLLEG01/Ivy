//! Retained prepare/apply/receipt protocol. Store adapters validate exact domain
//! schemas; this layer preserves original JSON values and destination identities.
use crate::journal_store::persistent_reads::json_values::Value;
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};

#[path = "decoded_mutations.rs"]
pub(crate) mod decoded;

#[path = "local_operations.rs"]
pub(crate) mod local;
pub(crate) use local::apply_graph;

#[derive(Debug, Clone)]
pub struct Document {
    pub pin: Json,
    pub value: Json,
    pub parent_id: Option<JsString>,
    pub content_hash: String,
}

/// The journal implementation owns CAS, schema validation, mutation receipts and
/// local/remote routing. No network effect may be inferred from a failed call.
#[allow(async_fn_in_trait)]
pub trait Store {
    fn root_object_id(&self) -> Option<JsString>;
    fn contract_version(&self, key: &str) -> IvyResult<String>;
    fn retired_operation(&self, caller: &JsString, id: &JsString) -> IvyResult<bool>;
    fn retired_operation_receipt(&self, _: &JsString, _: &JsString) -> IvyResult<Option<Json>> {
        Ok(None)
    }
    fn local_key(&self, id: &JsString) -> IvyResult<Option<String>>;
    fn complete_local_workflow(&self, id: &JsString, at: &JsString) -> IvyResult<()>;
    async fn named(
        &self,
        key: &str,
        name: &str,
        parent: Option<&JsString>,
    ) -> IvyResult<Option<Document>>;
    /// Address is either an object ID string or an exact object/revision pin.
    async fn read(&self, key: &str, address: &Json, upgrade: bool) -> IvyResult<Document>;
    async fn write(
        &self,
        key: &str,
        value: &Json,
        mutation: &str,
        destination: &Json,
    ) -> IvyResult<Json>;
    async fn prepared_mutation_id(&self, mutation: &str) -> IvyResult<JsString>;
    async fn write_prepared(
        &self,
        key: &str,
        value: &Json,
        mutation: &JsString,
        destination: &Json,
    ) -> IvyResult<Json>;
}

pub struct Operations<S, C> {
    pub store: S,
    /// Inject the owner's ISO UTC clock; called at the same persisted transitions as TS.
    pub now: C,
}

pub fn identity_hash(caller: &JsString, id: &JsString) -> IvyResult<String> {
    local::identity_hash(&Value::String(caller.clone()), &Value::String(id.clone()))
}
pub fn mutation(scope: &JsString, step: &str) -> IvyResult<String> {
    let Value::String(ref value) =
        local::mutation(&Value::String(scope.clone()), local::text(step))?
    else {
        unreachable!()
    };
    value.to_utf8()
}
impl<S: Store, C: Fn() -> String> Operations<S, C> {
    pub(crate) fn decoded(&self) -> Operations<decoded::DecodedStore<'_, S>, &C> {
        Operations {
            store: decoded::DecodedStore(&self.store),
            now: &self.now,
        }
    }
    pub async fn find(&self, caller: &JsString, id: &JsString) -> IvyResult<Option<Document>> {
        local::find_graph(
            &self.decoded(),
            &Value::String(caller.clone()),
            &Value::String(id.clone()),
        )
        .await
        .map_err(decoded::failure)?
        .map(decoded::document)
        .transpose()
    }
    pub async fn begin(&self, request: &Json, actor: &Json) -> IvyResult<Document> {
        decoded::document_result(
            local::begin_graph(&self.decoded(), &(request).clone(), &(actor).clone()).await,
        )
    }
    /// Bind completion identity only on first admission. Replays keep the
    /// original request and binding without repeating validation or task reads.
    pub async fn begin_with_handoff<F: std::future::Future<Output = IvyResult<Json>>>(
        &self,
        request: &Json,
        actor: &Json,
        handoff: impl FnOnce() -> F,
    ) -> IvyResult<Document> {
        decoded::document_result(
            local::begin_graph_with(
                &self.decoded(),
                request,
                actor,
                || async { handoff().await.map_err(Into::into) },
                &Value::Null,
            )
            .await,
        )
    }
    pub async fn prepare_native(&self, id: &JsString, proposed: &Json) -> IvyResult<Json> {
        decoded::json_result(
            local::prepare_native_graph(
                &self.decoded(),
                &Value::String(id.clone()),
                &(proposed).clone(),
            )
            .await,
        )
    }
    pub async fn observe_native(
        &self,
        id: &JsString,
        key: &JsString,
        observation: &Json,
    ) -> IvyResult<Json> {
        decoded::json_result(
            local::observe_native_graph(
                &self.decoded(),
                &Value::String(id.clone()),
                &Value::String(key.clone()),
                &(observation).clone(),
            )
            .await,
        )
    }
    pub async fn archive_identity(&self, id: &JsString) -> IvyResult<JsString> {
        decoded::string_result(
            local::archive_identity_graph(&self.decoded(), &Value::String(id.clone())).await,
        )
    }
    pub async fn stage(
        &self,
        id: &JsString,
        index: f64,
        key: &str,
        destination: &Json,
        proposed: &Json,
    ) -> IvyResult<Json> {
        decoded::json_result(
            local::stage_graph_with(
                &self.decoded(),
                &Value::String(id.clone()),
                index,
                key,
                &(destination).clone(),
                || std::future::ready(Ok((proposed).clone())),
            )
            .await,
        )
    }
    pub async fn apply(&self, id: &JsString, index: usize) -> IvyResult<Json> {
        decoded::json_result(apply_graph(&self.decoded(), &Value::String(id.clone()), index).await)
    }
    pub async fn finish(&self, id: &JsString, outcome: &Json) -> IvyResult<Json> {
        decoded::json_result(
            local::finish_graph(
                &self.decoded(),
                &Value::String(id.clone()),
                &(outcome).clone(),
            )
            .await,
        )
    }
    pub async fn failure(
        &self,
        id: &JsString,
        error: &IvyError,
        needs_attention: bool,
    ) -> IvyResult<Document> {
        decoded::document_result(
            local::failure_graph(
                &self.decoded(),
                &Value::String(id.clone()),
                error,
                needs_attention,
            )
            .await,
        )
    }
}
