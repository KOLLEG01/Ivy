//! Channel-scoped, content-free input lookup before reading a selected revision.
use crate::{
    admission::expected_bridge,
    operations::ChatOperations,
    store::{Pin, Reference},
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, chat_schema::validate_chat};
use ivy_sdk::rpc::RequestOptions;

fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn eq(field: &str, value: Json) -> Json {
    Json::object([
        ("op", "eq".into()),
        ("field", field.into()),
        ("value", value),
    ])
}
fn predicates(op: &str, values: Vec<Json>) -> Json {
    Json::object([("op", op.into()), ("args", Json::Array(values))])
}
pub struct ChatQueries<'a> {
    pub operations: &'a ChatOperations<'a>,
}
impl<'a> ChatQueries<'a> {
    pub fn new(operations: &'a ChatOperations<'a>) -> Self {
        Self { operations }
    }
    pub async fn input(&self, caller: &JsString, request: &Json) -> IvyResult<Json> {
        let admission = self.operations.admission;
        let store = self.operations.store;
        let expected = expected_bridge(request)?;
        admission.authorize(caller, expected, None)?;
        validate_chat("InputQuery", request)?;
        let configured = admission.channels_for(caller);
        let mut channels = Vec::new();
        for value in configured.as_array().ok_or_else(IvyError::internal)? {
            channels.push(predicates(
                "and",
                value
                    .required("channel")?
                    .as_object()
                    .ok_or_else(IvyError::internal)?
                    .iter()
                    .map(|(key, item)| {
                        Ok(eq(
                            &format!("data:/identity/channel/{}", key.to_utf8()?),
                            (*item).clone(),
                        ))
                    })
                    .collect::<IvyResult<Vec<_>>>()?,
            ));
        }
        let root = match &store.root_object_id {
            Some(root) => eq("object.parentId", root.clone().into()),
            None => Json::object([("op", "isNull".into()), ("field", "object.parentId".into())]),
        };
        let mut reference = None;
        // Preserve the original grouping of eight complete channel conjunctions.
        // The current Definition contract admits one configured channel.
        for group in channels.chunks(8) {
            let page = store
                .client
                .request(
                    "objects.query",
                    Json::object([
                        ("contractKey", "chat-bridge/input".into()),
                        ("limit", Json::Number(1.0)),
                        (
                            "where",
                            predicates(
                                "and",
                                vec![
                                    root.clone(),
                                    eq("object.id", request.required("inputId")?.clone()),
                                    eq(
                                        "data:/workspaceId",
                                        admission.definition().required("workspaceId")?.clone(),
                                    ),
                                    eq("data:/definitionHash", admission.definition_hash().into()),
                                    predicates("or", group.to_vec()),
                                ],
                            ),
                        ),
                    ]),
                    RequestOptions::default(),
                )
                .await?;
            require(
                !page.get("nextCursor").is_some_and(|value| !value.is_null()),
                "chat_identity_conflict",
                "An exact input identity must resolve to one Object.",
            )?;
            let items = page
                .get("items")
                .and_then(Json::as_array)
                .ok_or_else(IvyError::internal)?;
            if let Some(item) = items.first().filter(|v| !(v).is_null()) {
                reference = Some(Pin {
                    object_id: item.required("objectId")?.as_str()?,
                    revision: item
                        .required("revision")?
                        .as_f64()
                        .ok_or_else(IvyError::internal)? as i64,
                });
                break;
            }
        }
        require(
            reference.is_some(),
            "chat_input_unavailable",
            "The selected input is unavailable in the caller's admitted channels.",
        )?;
        let input = store
            .read("chat-bridge/input", Reference::Pinned(&reference.unwrap()))
            .await
            .map_err(|e| e.into_ivy())?;
        let identity = input
            .value
            .get("identity")
            .filter(|v| !v.is_null())
            .ok_or_else(IvyError::internal)?;
        admission.authorize(
            caller,
            expected,
            identity.get("channel").filter(|v| !(v).is_null()),
        )?;
        require(
            input.value.get("workspaceId") == admission.definition().get("workspaceId")
                && input.value.get("definitionHash")
                    == Some(&Json::from(admission.definition_hash())),
            "chat_scope_mismatch",
            "Input queries require the original configured workspace.",
        )?;
        Ok(Json::object([
            ("object", input.pin.json()),
            ("data", input.value),
        ]))
    }
}
