//! Exact retained native plans and invocation preflight. Parameter references are
//! inline values; these helpers do not perform RPC/storage or grant authority.
use crate::native_parameters::{
    NATIVE_PARAMETERS_BYTES, read_native_parameters, save_native_parameters,
};
use crate::{IvyError, IvyJson, IvyResult, JsString};
use ivy_contracts::{
    agent_schema::validate_agent, json::canonical, native_contract::NativeContract,
};

#[derive(Clone, Copy)]
pub enum NativePlanKind {
    TaskBoard,
    Chat,
}
const PARAMETERS: [(&str, &str); 3] = [
    ("threadStart", "thread/start"),
    ("threadResume", "thread/resume"),
    ("turnStart", "turn/start"),
];
fn reserved(key: &str, kind: NativePlanKind) -> Vec<JsString> {
    let names: &[&str] = match key {
        "threadStart" => &[],
        "threadResume" => &["threadId", "history", "path"],
        _ if matches!(kind, NativePlanKind::Chat) => &["threadId", "input", "clientUserMessageId"],
        _ => &["threadId"],
    };
    names.iter().map(|name| JsString::from(*name)).collect()
}
pub fn validate_native_invocation(
    contract: &NativeContract,
    value: &IvyJson,
    maximum_bytes: usize,
) -> IvyResult<()> {
    validate_agent("InvocationDraft", value)?;
    if value.get("nativeVersion") != contract.catalog().get("version")
        || value.get("catalogSourceHash") != contract.catalog().get("sourceHash")
    {
        return Err(IvyError::new(
            "native_request_catalog_mismatch",
            "Native invocation requires its exact retained catalog.",
        ));
    }
    contract.validate_input(
        value.get("method").unwrap().as_js_string().unwrap(),
        value.get("params").unwrap(),
        maximum_bytes,
    )
}
pub fn validate_native_plan_draft(
    contract: &NativeContract,
    draft: &IvyJson,
    kind: NativePlanKind,
) -> IvyResult<()> {
    validate_agent("PlanDraft", draft)?;
    canonical(draft, NATIVE_PARAMETERS_BYTES)?;
    validate_native_plan_identity(contract, draft)?;
    if let Some(location) = draft.get("location") {
        for (index, (key, _)) in PARAMETERS.iter().enumerate() {
            let cwd = draft.get(key).unwrap().get("cwd");
            let selected = location.get("cwd");
            if (index == 0 && cwd != selected)
                || (cwd.is_some_and(|cwd| !cwd.is_null()) && cwd != selected)
            {
                return Err(IvyError::new(
                    "native_plan_location_mismatch",
                    "Native parameters must retain the concrete selected project cwd.",
                ));
            }
        }
    }
    for (key, method) in PARAMETERS {
        contract.validate_template(
            &method.into(),
            draft.get(key).unwrap(),
            &reserved(key, kind),
            NATIVE_PARAMETERS_BYTES,
        )?;
    }
    Ok(())
}
pub fn save_native_plan(
    contract: &NativeContract,
    draft: &IvyJson,
    kind: NativePlanKind,
    parent_id: Option<&JsString>,
    _mutation_id: &JsString,
) -> IvyResult<IvyJson> {
    let mut plan = draft.clone();
    validate_native_plan_draft(contract, &plan, kind)?;
    for (key, method) in PARAMETERS {
        let reference = save_native_parameters(
            contract,
            &method.into(),
            draft.get(key).unwrap(),
            &reserved(key, kind),
            parent_id,
        )?;
        plan.insert(key, reference);
    }
    validate_agent("Plan", &plan)?;
    Ok(plan)
}
pub fn read_native_plan(
    contract: &NativeContract,
    plan: &IvyJson,
    kind: NativePlanKind,
    parent_id: Option<&JsString>,
) -> IvyResult<IvyJson> {
    let mut draft = plan.clone();
    validate_agent("Plan", &draft)?;
    validate_native_plan_identity(contract, &draft)?;
    for (key, method) in PARAMETERS {
        let value = read_native_parameters(contract, &method.into(), plan.get(key).unwrap(), &reserved(key, kind), parent_id)?;
        draft.insert(key, value);
    }
    validate_native_plan_draft(contract, &draft, kind)?;
    Ok(draft)
}

/// Check the selected catalog and every original method definition, in the
/// same order as the SDK. This does not validate the plan's parameter templates.
pub fn validate_native_plan_identity(contract: &NativeContract, plan: &IvyJson) -> IvyResult<()> {
    if plan.is_null() {
        return Err(IvyError::internal());
    }
    if plan.get("nativeVersion") != contract.catalog().get("version")
        || plan.get("catalogSourceHash") != contract.catalog().get("sourceHash")
    {
        return Err(IvyError::new(
            "native_plan_catalog_mismatch",
            "The plan must retain its selected native catalog source and version.",
        ));
    }
    let definitions = plan
        .get("definitions")
        .filter(|v| !v.is_null())
        .ok_or_else(IvyError::internal)?;
    for (key, method) in [
        ("threadStart", "thread/start"),
        ("threadResume", "thread/resume"),
        ("turnStart", "turn/start"),
        ("turnInterrupt", "turn/interrupt"),
    ] {
        let expected = JsString::from(contract.definition_hash(&method.into())?);
        if definitions.get(key).and_then(IvyJson::as_js_string) != Some(&expected) {
            return Err(IvyError::new(
                "native_plan_definition_mismatch",
                "The plan must retain every original native method definition.",
            ));
        }
    }
    Ok(())
}
