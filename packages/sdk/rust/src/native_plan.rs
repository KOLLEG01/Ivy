//! Identity checks for retained native plans. Draft/template admission is separate.
use crate::{IvyError, IvyJson, IvyResult, JsString};
use ivy_contracts::native_contract::NativeContract;

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
