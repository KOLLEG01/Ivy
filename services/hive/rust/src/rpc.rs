//! Hive's JSON-RPC reply and correlation boundary.
use ivy_contracts::{
    IvyError, IvyJson,
    validation::{safe_integer, text},
};

pub fn valid_request_id(value: Option<&IvyJson>) -> IvyJson {
    if text(value, 256) || safe_integer(value, 0.0) {
        value.unwrap().clone()
    } else {
        IvyJson::Null
    }
}

pub fn failure(error: &IvyError, id: IvyJson) -> IvyJson {
    IvyJson::object([
        ("jsonrpc", "2.0".into()),
        ("id", id),
        ("error", error.to_wire()),
    ])
}
