use crate::{
    IvyError, IvyJson, IvyResult, JsString,
    native_project_path::{strip_extended, windows_path},
};

pub fn normalized_project_path(value: &JsString) -> JsString {
    let mut slash = strip_extended(&value.0);
    for unit in &mut slash {
        if *unit == 92 {
            *unit = 47;
        }
    }
    while slash.last() == Some(&47) {
        slash.pop();
    }
    if windows_path(&value.0) || slash.starts_with(&[47, 47]) {
        JsString(slash).to_english_lowercase()
    } else {
        JsString(slash)
    }
}
/// Explicit membership wins, including a present null or empty project ID.
pub fn native_project_for_thread(
    projects: &[IvyJson],
    thread: &IvyJson,
) -> IvyResult<Option<IvyJson>> {
    if let Some(id) = thread
        .get("projectId")
        .and_then(IvyJson::as_js_string)
        .filter(|s| !s.0.is_empty())
    {
        return Ok(projects
            .iter()
            .find(|p| p.get("nativeId").and_then(IvyJson::as_js_string) == Some(id))
            .cloned());
    }
    if thread.get("projectId").is_some() {
        return Ok(None);
    }
    let Some(cwd) = thread.get("cwd").and_then(IvyJson::as_js_string) else {
        return Ok(None);
    };
    let child = normalized_project_path(cwd);
    let mut selected = None;
    let mut length = 0;
    for project in projects {
        for path in project
            .required("paths")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            let root = normalized_project_path(path.as_js_string().ok_or_else(IvyError::internal)?);
            let matches = child == root
                || child.0.starts_with(&root.0) && child.0.get(root.0.len()) == Some(&47);
            if matches && (selected.is_none() || root.0.len() > length) {
                selected = Some(project.clone());
                length = root.0.len();
            }
        }
    }
    Ok(selected)
}
pub fn is_internal_project(project: &IvyJson) -> bool {
    project.get("name") == Some(&IvyJson::from("IvyInternal"))
}
