//! Hive's offline owner binding. Host supplies an admitted InstanceConfig after
//! configuration/runtime/artifact checks and establishes stopped-writer ownership.
use crate::storage_read::inspect_stopped_storage;
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, core_schema::validate_host, json::parse,
};
use ivy_host_runtime::node_path::{self, Style};

pub fn inspect_admitted_configuration(config: &IvyJson, args: &[JsString]) -> IvyResult<IvyJson> {
    validate_host("InstanceConfig", config)?;
    if config.get("componentId") != Some(&"hive".into()) {
        return Err(IvyError::new(
            "target_conflict",
            "Only the Hive artifact may inspect its own storage.",
        ));
    }
    let contracts = if let Some(position) = args
        .iter()
        .position(|arg| arg == &JsString::from("--contracts"))
    {
        let encoded = args.get(position + 1).ok_or_else(IvyError::internal)?;
        parse(&String::from_utf16_lossy(&encoded.0), usize::MAX)
            .map_err(|_| IvyError::internal())?
    } else {
        IvyJson::Array(vec![])
    };
    let requirements = contracts.as_array().ok_or_else(|| {
        IvyError::new(
            "invalid_arguments",
            "Contract read domains must be a JSON array.",
        )
    })?;
    let root = config
        .get("dataRoot")
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)?;
    let path = node_path::join(Style::native(), &[root.clone(), "hive.sqlite".into()]);
    let result = inspect_stopped_storage(&node_path::filesystem_path(&path), requirements)?;
    validate_host("StorageInspection", &result)?;
    Ok(result)
}
