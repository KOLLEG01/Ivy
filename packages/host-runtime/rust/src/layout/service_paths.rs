use super::*;
pub(crate) fn text(value: &IvyJson) -> IvyResult<JsString> {
    value.as_js_string().cloned().ok_or_else(IvyError::internal)
}
pub(crate) fn joined(style: Style, parts: &[IvyJson]) -> IvyResult<IvyJson> {
    Ok(IvyJson::String(node_path::join(
        style,
        &parts.iter().map(text).collect::<IvyResult<Vec<_>>>()?,
    )))
}
pub(crate) fn service_base(
    config: &IvyJson,
    instance: &IvyJson,
    style: Style,
) -> IvyResult<IvyJson> {
    match config
        .get("servicesRoot")
        .filter(|v| v.as_js_string().is_some_and(|s| !s.0.is_empty()))
    {
        Some(root) => joined(
            style,
            &[root.clone(), instance.required("componentId")?.clone()],
        ),
        None => joined(
            style,
            &[
                config.required("runtimeRoot")?.clone(),
                "instances".into(),
                instance.required("instanceId")?.clone(),
            ],
        ),
    }
}
/// `instance` is either a configured instance object or its instance ID.
pub fn service_paths(config: &IvyJson, instance: &IvyJson) -> IvyResult<IvyJson> {
    service_paths_for_style(config, instance, Style::native())
}
pub fn service_paths_for_style(
    config: &IvyJson,
    instance: &IvyJson,
    style: Style,
) -> IvyResult<IvyJson> {
    let instance = if instance.as_js_string().is_some() {
        config
            .required("instances")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .find(|v| v.get("instanceId") == Some(instance))
            .ok_or_else(|| {
                IvyError::new("not_found", "Unknown service instance for path resolution.")
            })?
    } else {
        instance
    };
    let base = service_base(config, instance, style)?;
    if let Some(paths) = instance.get("paths").filter(|v| !v.is_null()) {
        return Ok(paths.clone());
    }
    Ok(IvyJson::object([
        ("data", joined(style, &[base.clone(), "data".into()])?),
        ("work", joined(style, &[base.clone(), "work".into()])?),
        ("logs", joined(style, &[base, "logs".into()])?),
    ]))
}
