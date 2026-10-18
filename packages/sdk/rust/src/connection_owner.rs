use crate::{
    IvyJson, IvyResult, JsString,
    rpc::{RequestOptions, RpcClient},
};
/// Bound ownership is authenticated once per provider connection, not used as live status.
pub async fn connection_owner(
    client: &dyn RpcClient,
    service_node_id: &JsString,
) -> IvyResult<IvyJson> {
    if let Some(connection) = client
        .service_connection()
        .filter(|c| c.service_node_id() == service_node_id)
    {
        return connection.bound_owner().await;
    }
    client
        .request(
            "serviceNodes.get",
            IvyJson::object([("serviceNodeId", IvyJson::String(service_node_id.clone()))]),
            RequestOptions::default(),
        )
        .await
}
