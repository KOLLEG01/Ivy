use crate::{
    IvyError, IvyJson, IvyResult, JsString,
    operation_ids::scoped_operation_id,
    rpc::{RequestOptions, RpcClient},
};
pub use ivy_contracts::browser_notice::{browser_notice_schema, browser_notice_topic};
use std::sync::Arc;

/// Publish original service content with UTF-16 truncation and the original scoped identity.
pub async fn publish_browser_notice(
    client: Arc<dyn RpcClient>,
    namespace: &JsString,
    notice: &IvyJson,
) -> IvyResult<()> {
    let mut payload = notice.clone();
    for (key, maximum) in [("title", 100), ("body", 300)] {
        let text = notice
            .required(key)?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        payload.insert(
            key,
            IvyJson::String(JsString(text.0[..text.0.len().min(maximum)].to_vec())),
        );
    }
    let scope = IvyJson::Array(vec![
        IvyJson::String(namespace.clone()),
        "browser-notification".into(),
        notice.required("tag")?.clone(),
    ]);
    let mutation_id = scoped_operation_id(client.clone(), &scope).await?;
    let mut topic = namespace.clone();
    topic.0.extend(".browser-notification".encode_utf16());
    client
        .request(
            "events.publish",
            IvyJson::object([
                ("topic", IvyJson::String(topic)),
                ("topicVersion", "1.0.0".into()),
                ("payload", payload),
                ("mutationId", IvyJson::String(mutation_id)),
            ]),
            RequestOptions::default(),
        )
        .await?;
    Ok(())
}
