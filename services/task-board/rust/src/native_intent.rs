//! Immutable native request parameters and the identity used for saved evidence.
//! These helpers neither dispatch native work nor authorize a replacement run.
use crate::{
    native_plan::catalog,
    operations::{Store, mutation},
    read_evidence::ReadEvidenceStore,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, checked_native_contract::CheckedNativeContracts,
};
use ivy_sdk::{
    native_parameters::{NATIVE_PARAMETERS_BYTES, read_native_parameters, save_native_parameters},
    native_plan::validate_native_invocation,
};

#[derive(Clone, Debug)]
pub struct EvidenceIdentity {
    pub operation_id: JsString,
    pub caller_principal_id: JsString,
    pub service_node_id: JsString,
    pub native_version: JsString,
    pub method: JsString,
    pub params: Json,
}
impl EvidenceIdentity {
    pub fn to_json(&self) -> Json {
        Json::object([
            ("operationId", Json::String(self.operation_id.clone())),
            (
                "callerPrincipalId",
                Json::String(self.caller_principal_id.clone()),
            ),
            ("serviceNodeId", Json::String(self.service_node_id.clone())),
            ("nativeVersion", Json::String(self.native_version.clone())),
            ("method", Json::String(self.method.clone())),
            ("params", self.params.clone()),
        ])
    }
}

pub struct NativeIntent<'a, S> {
    pub store: &'a S,
    pub catalogs: &'a CheckedNativeContracts,
}
fn string<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}

impl<S: Store + ReadEvidenceStore> NativeIntent<'_, S> {
    pub fn retain_native_request(
        &self,
        request: &Json,
        operation_id: &JsString,
    ) -> IvyResult<Json> {
        let contract = catalog(self.catalogs, request)?;
        validate_native_invocation(&contract, request, NATIVE_PARAMETERS_BYTES)?;
        let parent = self.store.root_object_id();
        // Preserve the original hash/error boundary even though inline parameter
        // references do not publish an Object or consume this mutation identity.
        mutation(operation_id, "native-parameters")?;
        let params = save_native_parameters(
            &contract,
            string(request, "method")?,
            request.required("params")?,
            &[],
            parent.as_ref(),
        )?;
        let mut retained = request.clone();
        retained.insert("params", params);
        Ok(retained)
    }

    pub fn resolve_native_request(&self, request: &Json) -> IvyResult<Json> {
        self.store.validate("NativeRequest", request)?;
        let contract = catalog(self.catalogs, request)?;
        let params = read_native_parameters(
            &contract,
            string(request, "method")?,
            request.required("params")?,
            &[],
            self.store.root_object_id().as_ref(),
        )?;
        let mut resolved = request.clone();
        resolved.insert("params", params);
        validate_native_invocation(&contract, &resolved, NATIVE_PARAMETERS_BYTES)?;
        Ok(resolved)
    }

    pub fn evidence_identity(
        &self,
        run: &Json,
        call: &Json,
        principal_id: &JsString,
    ) -> IvyResult<EvidenceIdentity> {
        let request = call.required("request")?;
        let operation_id = string(call, "operationId")?.clone();
        let service_node_id = string(run.required("target")?, "serviceNodeId")?.clone();
        let native_version = string(request, "nativeVersion")?.clone();
        let method = string(request, "method")?.clone();
        let resolved = self.resolve_native_request(request)?;
        Ok(EvidenceIdentity {
            operation_id,
            caller_principal_id: principal_id.clone(),
            service_node_id,
            native_version,
            method,
            params: resolved.required("params")?.clone(),
        })
    }
}
