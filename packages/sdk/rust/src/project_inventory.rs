use crate::{
    IvyError, IvyJson, IvyResult, JsString,
    rpc::{RequestOptions, RpcClient},
};

/// Retained project identities belong to their owning host, including offline hosts.
pub async fn host_project(
    client: &dyn RpcClient,
    host_id: Option<&JsString>,
    project_id: &JsString,
    options: RequestOptions,
) -> IvyResult<Option<IvyJson>> {
    let mut cursor = None;
    loop {
        let mut params = IvyJson::object([
            ("serviceName", "agent-manager".into()),
            ("namespace", "codex".into()),
            ("kind", "project".into()),
        ]);
        if let Some(host) = host_id.filter(|value| !value.0.is_empty()) {
            params.insert("hostId", IvyJson::String(host.clone()));
        }
        params.insert("limit", IvyJson::Number(100.0));
        if let Some(value) = cursor {
            params.insert("cursor", IvyJson::String(value));
        }
        let page = client
            .request("inventory.list", params, options.clone())
            .await?;
        for item in page
            .required("items")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            if item
                .get("resourceRef")
                .and_then(|value| value.get("nativeId"))
                .and_then(IvyJson::as_js_string)
                != Some(project_id)
            {
                continue;
            }
            if let Some(value) = item
                .get("summary")
                .filter(|value| value.as_object().is_some())
            {
                if value.get("nativeId").and_then(IvyJson::as_js_string) == Some(project_id)
                    && value.get("name").and_then(IvyJson::as_js_string).is_some()
                    && value.get("paths").and_then(IvyJson::as_array).is_some()
                {
                    return Ok(Some(value.clone()));
                }
            }
        }
        cursor = page
            .get("nextCursor")
            .and_then(IvyJson::as_js_string)
            .filter(|value| !value.0.is_empty())
            .cloned();
        if cursor.is_none() {
            return Ok(None);
        }
    }
}
