use ivy_contracts::{IvyJson, json::parse, native_contract::NativeContract};
use ivy_sdk::{
    native_evidence::{NativePayloadLimits, validate_native_payload},
    native_plan::{NativePlanKind, read_native_plan, save_native_plan},
};

fn catalog() -> NativeContract {
    let value = parse(include_str!("../../specs/native/codex-0.159.2/catalog.json"), usize::MAX).unwrap();
    NativeContract::new(&value).unwrap()
}
fn draft(contract: &NativeContract) -> IvyJson {
    let definitions = IvyJson::object([
        ("threadStart", "thread/start"),
        ("threadResume", "thread/resume"),
        ("turnStart", "turn/start"),
        ("turnInterrupt", "turn/interrupt"),
    ].map(|(key, method)| (key, contract.definition_hash(&method.into()).unwrap().into())));
    IvyJson::object([
        ("nativeVersion", contract.catalog().required("version").unwrap().clone()),
        ("catalogSourceHash", contract.catalog().required("sourceHash").unwrap().clone()),
        ("definitions", definitions),
        ("threadStart", IvyJson::object([] as [(&str, IvyJson); 0])),
        ("threadResume", IvyJson::object([] as [(&str, IvyJson); 0])),
        ("turnStart", IvyJson::object([] as [(&str, IvyJson); 0])),
    ])
}
fn observation() -> IvyJson {
    parse(r#"{"method":"turn/interrupt","params":{"threadId":"thread","turnId":"turn"},"requestId":"request","reply":{"result":{}}}"#, 4096).unwrap()
}
fn limits() -> NativePayloadLimits {
    NativePayloadLimits { request_bytes: 4096, response_bytes: 4096 }
}

#[test]
fn inline_plans_round_trip_and_keep_the_exact_catalog() {
    let contract = catalog();
    let input = draft(&contract);
    let plan = save_native_plan(&contract, &input, NativePlanKind::Chat, None, &"operation".into()).unwrap();
    assert_eq!(read_native_plan(&contract, &plan, NativePlanKind::Chat, None).unwrap(), input);
    assert_eq!(save_native_plan(&contract, &input, NativePlanKind::Chat, None, &"another-operation".into()).unwrap(), plan);
    let mut changed = plan.clone();
    let mut parameters = changed.required("threadResume").unwrap().clone();
    parameters.insert("catalogHash", format!("sha256:{}", "0".repeat(64)).into());
    changed.insert("threadResume", parameters);
    assert_eq!(read_native_plan(&contract, &changed, NativePlanKind::Chat, None).unwrap_err().code, "native_parameters_contract_mismatch");
}

#[test]
fn native_payload_uses_declared_parameters_and_object_replies() {
    let contract = catalog();
    let input = observation();
    validate_native_payload(&contract, &input, limits()).unwrap();
    for reply in [IvyJson::Bool(false), IvyJson::Number(0.0), "text".into(), IvyJson::Array(vec![])] {
        let mut changed = input.clone();
        changed.insert("reply", reply);
        assert_eq!(validate_native_payload(&contract, &changed, limits()).unwrap_err().code, "invalid_frame");
    }
    let mut changed = input;
    changed.insert("params", parse(r#"{"threadId":"thread"}"#, 1024).unwrap());
    assert!(validate_native_payload(&contract, &changed, limits()).is_err());
}

#[test]
fn native_payload_bounds_both_transport_envelopes() {
    let contract = catalog();
    let input = observation();
    assert!(validate_native_payload(&contract, &input, NativePayloadLimits { request_bytes: 1, response_bytes: 4096 }).is_err());
    assert!(validate_native_payload(&contract, &input, NativePayloadLimits { request_bytes: 4096, response_bytes: 1 }).is_err());
}
