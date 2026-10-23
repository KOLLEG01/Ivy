use crate::{
    IvyJson, IvyResult, config, node_path,
    portable_native_files::{NativeHelperLayout, component_native_helper},
};
use ivy_contracts::{JsString, core_schema::validate_host};
use std::path::Path;

pub(super) struct Worker {
    pub executable: JsString,
    pub args: Vec<JsString>,
}

/// Resolve the worker from the retained CURRENT artifact before OS admission.
/// The replacement candidate never provides its own maintenance executable.
pub(super) fn worker(plan: &IvyJson, request: &Path, name: &str) -> IvyResult<Worker> {
    validate_host("BootstrapPlan", plan)?;
    let artifact = config::path_value(plan.required("artifactRoot")?)?;
    let worker = component_native_helper(&artifact, name, NativeHelperLayout::Target)?;
    let mut args = vec![
        "--config".into(),
        text(plan, "configPath")?,
        "--request".into(),
        node_path::from_native(request),
        "--distribution".into(),
        text(plan, "artifactRoot")?,
    ];
    let needs_node = plan
        .required("processes")?
        .as_array()
        .ok_or_else(crate::IvyError::internal)?
        .iter()
        .any(|process| {
            process.get("entrypoint").map_or(true, |entry| {
                entry.get("executable") == Some(&"node".into())
            })
        });
    if needs_node {
        args.extend(["--node-executable".into(), text(plan, "nodeExecutable")?]);
    }
    Ok(Worker {
        executable: node_path::from_native(&worker),
        args,
    })
}
fn text(plan: &IvyJson, key: &str) -> IvyResult<JsString> {
    plan.required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(crate::IvyError::internal)
}
pub(super) fn windows_request(
    plan: IvyJson,
    request: std::path::PathBuf,
    operation: String,
) -> IvyResult<IvyJson> {
    let selected = worker(&plan, &request, "ivy-bootstrap-auto-worker.exe")?;
    let artifact = config::path_value(plan.required("artifactRoot")?)?;
    let launcher =
        component_native_helper(&artifact, "ivy-host-job.exe", NativeHelperLayout::Target)?;
    Ok(IvyJson::object([
        ("plan", plan),
        (
            "requestPath",
            IvyJson::String(node_path::from_native(&request)),
        ),
        ("operationId", operation.into()),
        (
            "nodeRequired",
            selected
                .args
                .iter()
                .any(|arg| arg == &JsString::from("--node-executable"))
                .into(),
        ),
        ("workerExecutable", IvyJson::String(selected.executable)),
        (
            "hostJobLauncher",
            IvyJson::String(node_path::from_native(&launcher)),
        ),
    ]))
}
