use crate::IvyJson;
use std::sync::OnceLock;

pub fn is_read_request(method:&str,params:&IvyJson)->bool {
    if method=="notifications.subscribe" { return false; }
    if matches!(method,"tools.call"|"discovery.call") { return params.get("operationId").is_none(); }
    static CATALOG:OnceLock<serde_json::Value>=OnceLock::new();
    let catalog=CATALOG.get_or_init(||serde_json::from_str(include_str!("../../../../specs/schemas/operations.json")).expect("checked-in operation catalog"));
    catalog["operations"].get(method).is_some_and(|definition|definition["access"]=="client"&&definition["mutation"]==false)
}
