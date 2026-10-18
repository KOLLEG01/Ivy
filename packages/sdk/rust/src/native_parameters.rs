//! Inline immutable native parameter references. The current SDK does not make
//! RPC calls or publish parameter Objects; workflow policy stays with callers.
use crate::{IvyError, IvyJson, IvyResult, JsString};
use ivy_contracts::{
    agent_schema::validate_agent, json::canonical, native_contract::NativeContract,
    schema::SchemaValidators,
};
use std::sync::OnceLock;

pub const NATIVE_PARAMETERS_BYTES: usize = 1_048_576;

fn names(reserved: &[JsString]) -> Vec<JsString> {
    let mut names = reserved.to_vec();
    names.sort();
    names.dedup();
    names
}
fn header(contract: &NativeContract, method: &JsString, reserved: &[JsString]) -> IvyJson {
    IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        (
            "nativeVersion",
            contract.catalog().get("version").unwrap().clone(),
        ),
        (
            "nativeExecutableHash",
            contract
                .catalog()
                .get("nativeExecutableHash")
                .unwrap()
                .clone(),
        ),
        ("catalogHash", contract.catalog_hash().into()),
        ("method", IvyJson::String(method.clone())),
        (
            "reservedFields",
            IvyJson::Array(names(reserved).into_iter().map(IvyJson::String).collect()),
        ),
    ])
}
fn parameters_schema(
    contract: &NativeContract,
    method: &JsString,
    reserved: &[JsString],
) -> IvyResult<()> {
    let input = contract.input_schema(method, reserved)?;
    let mut params = input.as_ref().clone();
    let definitions = params.remove("$defs");
    let identity = header(contract, method, reserved);
    let mut required = Vec::new();
    let mut properties = IvyJson::Object(vec![]);
    for (key, value) in identity.as_object().unwrap() {
        properties.insert(key.clone(), IvyJson::object([("const", value.clone())]));
        required.push(IvyJson::String(key.clone()));
    }
    properties.insert("params", params);
    required.push("params".into());
    let mut schema = IvyJson::object([
        ("type", "object".into()),
        ("properties", properties),
        ("required", IvyJson::Array(required)),
        ("additionalProperties", false.into()),
    ]);
    if let Some(definitions) = definitions {
        schema.insert("$defs", definitions);
    }
    static VALIDATORS: OnceLock<SchemaValidators> = OnceLock::new();
    VALIDATORS
        .get_or_init(SchemaValidators::new)
        .compile(&schema)?;
    Ok(())
}

/// The parent identity is retained in the call surface; the original inline
/// format does not consult it or perform storage I/O.
pub fn save_native_parameters(
    contract: &NativeContract,
    method: &JsString,
    params: &IvyJson,
    reserved: &[JsString],
    _parent_id: Option<&JsString>,
) -> IvyResult<IvyJson> {
    let reserved = names(reserved);
    let mut value = header(contract, method, &reserved);
    value.insert("params", params.clone());
    canonical(&value, NATIVE_PARAMETERS_BYTES)?;
    contract.validate_template(method, params, &reserved, NATIVE_PARAMETERS_BYTES)?;
    parameters_schema(contract, method, &reserved)?;
    validate_agent("ParametersRef", &value)?;
    Ok(value)
}
pub fn read_native_parameters(
    contract: &NativeContract,
    method: &JsString,
    value: &IvyJson,
    reserved: &[JsString],
    _parent_id: Option<&JsString>,
) -> IvyResult<IvyJson> {
    let reserved = names(reserved);
    let expected = header(contract, method, &reserved);
    validate_agent("ParametersRef", value)?;
    canonical(value, NATIVE_PARAMETERS_BYTES)?;
    parameters_schema(contract, method, &reserved)?;
    if [
        "nativeVersion",
        "nativeExecutableHash",
        "catalogHash",
        "method",
    ]
    .iter()
    .any(|key| value.get(key) != expected.get(key))
        || canonical(value.get("reservedFields").unwrap(), usize::MAX)?
            != canonical(expected.get("reservedFields").unwrap(), usize::MAX)?
    {
        return Err(IvyError::new(
            "native_parameters_contract_mismatch",
            "Native parameters belong to another selected catalog, method or template.",
        ));
    }
    let params = value.get("params").unwrap();
    contract.validate_template(method, params, &reserved, NATIVE_PARAMETERS_BYTES)?;
    Ok(params.clone())
}
