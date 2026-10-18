//! Page pending notice work, including recoverable uncertain display receipts.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::rpc::{RequestOptions, RpcClient};

fn equal(field: &str, value: IvyJson) -> IvyJson {
    IvyJson::object([
        ("op", "eq".into()),
        ("field", field.into()),
        ("value", value),
    ])
}
fn group(op: &str, args: Vec<IvyJson>) -> IvyJson {
    IvyJson::object([("op", op.into()), ("args", IvyJson::Array(args))])
}

#[derive(Default)]
pub struct NoticeScan {
    pub cursor: Option<JsString>,
}

impl NoticeScan {
    pub async fn page(
        &mut self,
        client: &dyn RpcClient,
        root: &JsString,
        limit: usize,
    ) -> IvyResult<Vec<JsString>> {
        let state = |value: &str| equal("data:/notice/state", value.into());
        let pending = group(
            "or",
            vec![
                state("queued"),
                state("dispatching"),
                group(
                    "and",
                    vec![
                        state("confirmed"),
                        group(
                            "or",
                            vec![
                                equal("data:/notice/voice/state", "queued".into()),
                                equal("data:/notice/voice/state", "outcome_unknown".into()),
                            ],
                        ),
                    ],
                ),
                // SEC02: only this uncertainty can be resolved by reading the original
                // display receipt. Other unknown outcomes must not regain dispatch.
                group(
                    "and",
                    vec![
                        state("outcome_unknown"),
                        equal("data:/notice/reason", "display_outcome_unknown".into()),
                    ],
                ),
            ],
        );
        let mut request = IvyJson::object([
            ("contractKey", "secretary/item".into()),
            ("limit", IvyJson::Number(limit as f64)),
            (
                "orderBy",
                IvyJson::Array(vec![IvyJson::object([
                    ("field", "object.id".into()),
                    ("direction", "asc".into()),
                ])]),
            ),
            (
                "where",
                group(
                    "and",
                    vec![
                        equal("object.parentId", IvyJson::String(root.clone())),
                        pending,
                    ],
                ),
            ),
        ]);
        if let Some(cursor) = self.cursor.as_ref().filter(|v| !v.0.is_empty()) {
            request.insert("cursor", IvyJson::String(cursor.clone()));
        }
        let page = client
            .request("objects.query", request, RequestOptions::default())
            .await?;
        let cursor = match page.get("nextCursor").filter(|v| !v.is_null()) {
            Some(value) => Some(value.as_js_string().ok_or_else(IvyError::internal)?.clone()),
            None => None,
        };
        let ids = page
            .required("items")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .map(|row| {
                Ok(row
                    .required("objectId")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?
                    .clone())
            })
            .collect::<IvyResult<Vec<_>>>()?;
        self.cursor = cursor;
        Ok(ids)
    }
}
