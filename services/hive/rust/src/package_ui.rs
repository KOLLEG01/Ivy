//! App-package publication through the canonical CLI scanner and SDK RPC owner.
use crate::{
    packages::{PackageInstallation, PackageStorage, PackageUiInstallation, PackageUiInstaller},
    store::sql_error,
    ui_files::UiFiles,
};
use ivy_cli::publish_ui::read_ui_bundle;
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, hash::digest, json::parse,
    operation::derive_operation_id_js,
};
use ivy_host_runtime::node_path::{self, Context};
use ivy_sdk::{
    operation_ids::new_operation_id,
    rpc::{RequestFuture, RequestOptions, RpcClient},
};
use std::sync::Arc;

/// Uses the same storage owner and private UI files as the package registry.
#[derive(Clone)]
pub struct UiPackageInstaller {
    worker: Arc<dyn PackageStorage>,
    files: Arc<UiFiles>,
}
impl UiPackageInstaller {
    pub fn new(worker: Arc<dyn PackageStorage>, files: Arc<UiFiles>) -> Self {
        Self { worker, files }
    }

    async fn publish(&self, input: PackageUiInstallation) -> IvyResult<()> {
        let manifest = &input.manifest;
        let app = manifest.get("app").filter(|app| !app.is_null());
        require(
            manifest.get("kind") == Some(&"app".into())
                && app.is_some_and(|app| app.get("appId") == manifest.get("componentId")),
            "build_mismatch",
            "App package manifest does not identify its app.",
        )?;
        let app = app.ok_or_else(IvyError::internal)?;
        let ui = string(app, "appId")?;
        let directory = node_path::to_native(&Context::current().map_err(sql_error)?.resolve(&[
            node_path::from_native(&input.artifact_root),
            string(app, "dist")?.clone(),
        ]));
        let bytes = tokio::fs::read(directory.join("ivy-ui.json"))
            .await
            .map_err(sql_error)?;
        let definition = parse(&String::from_utf8_lossy(&bytes), usize::MAX).map_err(sql_error)?;
        let metadata = definition
            .get("metadata")
            .filter(|value| !value.is_null())
            .ok_or_else(IvyError::internal)?;
        require(
            metadata.get("uiId") == app.get("appId")
                && definition.get("entryPath") == app.get("entryPath"),
            "build_mismatch",
            "UI bundle definition differs from its package manifest.",
        )?;

        // Each installation gets the source's fresh client and epoch-cache lifetime.
        let client: Arc<dyn RpcClient> = Arc::new(PublisherClient {
            worker: self.worker.clone(),
            principal: input.publisher_principal_id,
        });
        let scan_directory = directory.clone();
        let scan_definition = definition.clone();
        let bundle = tokio::task::spawn_blocking(move || {
            read_ui_bundle(&scan_directory, &scan_definition).map(|bundle| bundle.to_json())
        })
        .await
        .map_err(sql_error)??;
        let pointers = self
            .worker
            .request(IvyJson::object([
                ("action", "ui.pointers".into()),
                ("uiId", IvyJson::String(ui.clone())),
            ]))
            .await?;
        let expected = pointers
            .as_array()
            .ok_or_else(IvyError::internal)?
            .first()
            .and_then(|row| row.get("current_release_id"))
            .cloned()
            .unwrap_or(IvyJson::Null);
        let release = string(&bundle, "releaseId")?;
        let assets = bundle
            .required("assets")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        self.files.install(&directory, ui, release, assets).await?;
        if expected == IvyJson::String(release.clone()) {
            return Ok(());
        }

        let nonce = JsString(
            "package-"
                .encode_utf16()
                .chain(input.build_id.0.iter().skip(7).take(40).copied())
                .collect(),
        );
        let mutation = new_operation_id(client.clone(), Some(nonce)).await?;
        for contract in definition
            .required("dataContracts")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            require(
                contract.get("owner").and_then(|owner| owner.get("kind")) == Some(&"agent".into()),
                "contract_owner_mismatch",
                "UI publication can only register agent-owned data contracts.",
            )?;
            let scope = IvyJson::Array(vec![
                "contract".into(),
                contract.required("key")?.clone(),
                contract.required("version")?.clone(),
            ]);
            client
                .request(
                    "contracts.register",
                    IvyJson::object([
                        (
                            "mutationId",
                            IvyJson::String(derive_operation_id_js(&mutation, &scope)?),
                        ),
                        ("definition", contract.clone()),
                    ]),
                    RequestOptions::default(),
                )
                .await?;
        }
        client
            .request(
                "uis.deploy",
                IvyJson::object([
                    ("mutationId", IvyJson::String(mutation)),
                    ("expectedReleaseId", expected),
                    ("metadata", definition.required("metadata")?.clone()),
                    (
                        "release",
                        IvyJson::object([
                            ("releaseId", IvyJson::String(release.clone())),
                            ("entryPath", definition.required("entryPath")?.clone()),
                            ("requirements", definition.required("requirements")?.clone()),
                            ("assets", IvyJson::Array(assets.to_vec())),
                        ]),
                    ),
                ]),
                RequestOptions::default(),
            )
            .await?;
        Ok(())
    }
}
impl PackageUiInstaller for UiPackageInstaller {
    fn install(&self, input: PackageUiInstallation) -> PackageInstallation {
        let this = self.clone();
        Box::pin(async move { this.publish(input).await })
    }
}

struct PublisherClient {
    worker: Arc<dyn PackageStorage>,
    principal: JsString,
}
impl RpcClient for PublisherClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let mut identity = JsString::from("package-publisher:");
            identity.0.extend(&self.principal.0);
            self.worker
                .request(IvyJson::object([
                    ("action", "execute".into()),
                    (
                        "context",
                        IvyJson::object([
                            ("principalId", IvyJson::String(self.principal.clone())),
                            (
                                "credentialDigest",
                                digest(String::from_utf16_lossy(&identity.0).as_bytes()).into(),
                            ),
                            ("transport", "http".into()),
                        ]),
                    ),
                    (
                        "request",
                        IvyJson::object([
                            ("jsonrpc", "2.0".into()),
                            ("id", uuid::Uuid::new_v4().to_string().into()),
                            ("method", method.into()),
                            ("params", params),
                        ]),
                    ),
                ]))
                .await?
                .required("value")
                .cloned()
        })
    }
}
fn string<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    value
        .get(key)
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
