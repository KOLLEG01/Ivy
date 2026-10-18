//! Resolve saved TaskBoard plans against their exact retained native catalog.
use crate::operations::Store;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, checked_native_contract::CheckedNativeContracts,
    native_contract::NativeContract,
};
use ivy_sdk::native_plan::{NativePlanKind, read_native_plan};
use std::sync::Arc;

pub(crate) fn catalog(
    catalogs: &CheckedNativeContracts,
    value: &Json,
) -> IvyResult<Arc<NativeContract>> {
    if value.is_null() {
        return Err(IvyError::internal());
    }
    // Unsupported/missing versions must reach the shared version gate before
    // identity admission, like the original checkedNativeContract call.
    let version = value
        .get("nativeVersion")
        .and_then(Json::as_js_string)
        .cloned()
        .unwrap_or_default();
    let identity = Json::object([(
        "sourceHash",
        value
            .get("catalogSourceHash")
            .cloned()
            .unwrap_or(Json::Null),
    )]);
    catalogs.checked_native_contract(&version, Some(&identity))
}

pub fn resolved_plan<S: Store>(
    store: &S,
    catalogs: &CheckedNativeContracts,
    plan: &Json,
) -> IvyResult<Json> {
    let contract = catalog(catalogs, plan)?;
    read_native_plan(
        &contract,
        plan,
        NativePlanKind::TaskBoard,
        store.root_object_id().as_ref(),
    )
}

pub async fn read_plan<S: Store>(
    store: &S,
    catalogs: &CheckedNativeContracts,
    pin: &Json,
) -> IvyResult<Json> {
    let plan = store.read("task-board/native-plan", pin, true).await?;
    resolved_plan(store, catalogs, &plan.value)
}
