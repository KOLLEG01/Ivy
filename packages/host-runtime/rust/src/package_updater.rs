//! Catalog selection and immutable package import. Transport policy is supplied
//! by the composing host; admission, locking and journal replay stay here.
pub(crate) mod transfers;
use crate::{
    IvyError, IvyJson, IvyResult, artifact, config,
    executor_lock::ExecutorLock,
    journal::{HostJournal, requires_bootstrap_replacement},
    node_path,
    node_runtime::NodeRuntime,
    package_receive, package_validation, preparation_transfer,
};
use futures_util::{TryStreamExt, future::BoxFuture};
use ivy_contracts::{hash::hash_json, json};
use std::{cmp::Ordering, collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::{io::AsyncWriteExt, sync::Mutex};

pub trait PackageClient: Send + Sync {
    /// A 204 response is None; every other successful body is parsed as JSON.
    fn catalog(
        &self,
        base: String,
        credential: String,
        revision: f64,
    ) -> BoxFuture<'_, IvyResult<Option<IvyJson>>>;
    /// Download to a new file, retaining the original five-minute deadline.
    fn download(
        &self,
        base: String,
        credential: String,
        entry: IvyJson,
        archive: PathBuf,
    ) -> BoxFuture<'_, IvyResult<()>>;
}

/// Uses an explicitly composed client (TLS, proxy and decoding policy included).
/// This adapter does not claim equivalence to a different caller's fetch profile.
pub struct NativePackageClient(pub reqwest::Client);
impl NativePackageClient {
    pub fn new() -> IvyResult<Self> {
        reqwest::Client::builder()
            .build()
            .map(Self)
            .map_err(|_| IvyError::internal())
    }
}
fn endpoint(base: &str, relative: &str) -> IvyResult<reqwest::Url> {
    let mut base = reqwest::Url::parse(base).map_err(|_| IvyError::internal())?;
    let relative = reqwest::Url::parse(&format!("http://ivy.invalid{relative}"))
        .map_err(|_| IvyError::internal())?;
    let prefix = base.path().strip_suffix('/').unwrap_or(base.path());
    let path = format!("{prefix}{}", relative.path());
    base.set_path(&path);
    base.set_query(relative.query());
    base.set_fragment(None);
    Ok(base)
}
impl PackageClient for NativePackageClient {
    fn catalog(
        &self,
        base: String,
        credential: String,
        revision: f64,
    ) -> BoxFuture<'_, IvyResult<Option<IvyJson>>> {
        Box::pin(async move {
            let mut response = self
                .0
                .get(endpoint(
                    &base,
                    &format!("/api/v1/packages/catalog?after={revision}"),
                )?)
                .bearer_auth(credential)
                .timeout(Duration::from_secs(5))
                .send()
                .await
                .map_err(|_| IvyError::internal())?;
            if response.status() == 204 {
                return Ok(None);
            }
            require(
                response.status().is_success(),
                "service_unavailable",
                "Hive package catalog is unavailable.",
            )?;
            let maximum = package_validation::CATALOG_BYTES;
            require(response.content_length().is_none_or(|length| length <= maximum as u64), "limit_exceeded", "Package catalog exceeds its byte limit.")?;
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| IvyError::internal())? {
                require(chunk.len() <= maximum.saturating_sub(bytes.len()), "limit_exceeded", "Package catalog exceeds its byte limit.")?;
                bytes.extend_from_slice(&chunk);
            }
            let text = String::from_utf8_lossy(&bytes);
            let value = json::parse(text.strip_prefix('\u{feff}').unwrap_or(&text), maximum)
                .map_err(|_| IvyError::internal())?;
            Ok(Some(value))
        })
    }
    fn download(
        &self,
        base: String,
        credential: String,
        entry: IvyJson,
        archive: PathBuf,
    ) -> BoxFuture<'_, IvyResult<()>> {
        Box::pin(async move {
            // Both identities have already passed the catalog's ASCII grammar.
            let relative = format!(
                "/api/v1/packages/{}/{}/artifact",
                text(&entry, "componentId")?,
                text(&entry, "version")?
            );
            let response = self
                .0
                .get(endpoint(&base, &relative)?)
                .bearer_auth(credential)
                .timeout(Duration::from_secs(300))
                .send()
                .await
                .map_err(|_| IvyError::internal())?;
            let status = response.status();
            require(
                status.is_success() && status != 204 && status != 205,
                if status == 404 {
                    "not_found"
                } else {
                    "service_unavailable"
                },
                "Package artifact download failed.",
            )?;
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(archive)
                .await
                .map_err(|_| IvyError::internal())?;
            let mut body = response.bytes_stream();
            while let Some(bytes) = body.try_next().await.map_err(|_| IvyError::internal())? {
                file.write_all(&bytes)
                    .await
                    .map_err(|_| IvyError::internal())?;
            }
            file.flush().await.map_err(|_| IvyError::internal())
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BootstrapPackageComponent {
    pub instance_id: String,
    pub candidate: IvyJson,
    pub manifest: IvyJson,
}
impl BootstrapPackageComponent {
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            ("instanceId", self.instance_id.clone().into()),
            ("candidate", self.candidate.clone()),
            ("manifest", self.manifest.clone()),
        ])
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct PackageSyncResult {
    pub bootstrap: Vec<BootstrapPackageComponent>,
}
impl PackageSyncResult {
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([(
            "bootstrap",
            IvyJson::Array(
                self.bootstrap
                    .iter()
                    .map(BootstrapPackageComponent::to_json)
                    .collect(),
            ),
        )])
    }
}
#[derive(Default)]
struct State {
    revision: f64,
    pending: Vec<BootstrapPackageComponent>,
}
struct Inner {
    journal: HostJournal,
    credential: String,
    node: Option<NodeRuntime>,
    client: Arc<dyn PackageClient>,
    state: Mutex<State>,
}
#[derive(Clone)]
pub struct PackageUpdater(Arc<Inner>);
impl PackageUpdater {
    pub fn new(
        journal: HostJournal,
        credential: String,
        node: impl Into<Option<NodeRuntime>>,
        client: Arc<dyn PackageClient>,
    ) -> Self {
        Self(Arc::new(Inner {
            journal,
            credential,
            node: node.into(),
            client,
            state: Mutex::new(State::default()),
        }))
    }
    /// An admitted sync keeps its lock/import/journal ownership if its waiter is
    /// dropped. A subsequent sync observes the same revision and pending order.
    pub async fn sync(&self) -> IvyResult<PackageSyncResult> {
        let inner = self.0.clone();
        tokio::spawn(async move {
            let mut state = inner.state.lock().await;
            inner.sync(&mut state).await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
    pub async fn import_package(&self, entry: IvyJson) -> IvyResult<IvyJson> {
        package_validation::PackageIndex::read(&entry)?.validate()?;
        let inner = self.0.clone();
        tokio::spawn(async move { inner.import_package(&entry).await })
            .await
            .map_err(|_| IvyError::internal())?
    }
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn text(value: &IvyJson, key: &str) -> IvyResult<String> {
    value.required(key)?.as_str()
}
fn instances(value: &IvyJson) -> IvyResult<&[IvyJson]> {
    value
        .required("instances")?
        .as_array()
        .ok_or_else(IvyError::internal)
}
fn platform(node: Option<&NodeRuntime>) -> IvyJson {
    IvyJson::object([
        (
            "os",
            crate::native_plan::host_os()
            .into(),
        ),
        (
            "arch",
            match std::env::consts::ARCH {
                "x86_64" => "x64",
                "aarch64" => "arm64",
                value => value,
            }
            .into(),
        ),
        (
            "node",
            node.map(|node| {
                if node.version.starts_with('v') {
                    node.version.clone()
                } else {
                    format!("v{}", node.version)
                }
            })
            .unwrap_or_else(|| "unavailable".into())
            .into(),
        ),
    ])
}
fn supported(manifest: &IvyJson, node: Option<&NodeRuntime>) -> IvyResult<bool> {
    match artifact::verify_runtime_requirements(
        manifest,
        node.map(|node| node.version.as_str()),
        crate::native_plan::host_os(),
        match std::env::consts::ARCH { "x86_64" => "x64", "aarch64" => "arm64", arch => arch },
    ) {
        Ok(()) => Ok(true),
        Err(error) if error.code == "unsupported_runtime" => Ok(false),
        Err(error) => Err(error),
    }
}
async fn remove(path: PathBuf) -> IvyResult<()> {
    crate::filesystem::remove_force_raw(path)
        .await
        .map_err(|_| IvyError::internal())
}
async fn move_payload(source: PathBuf, destination: PathBuf, directory: bool) -> IvyResult<()> {
    let from = source.clone();
    let to = destination.clone();
    let result = tokio::task::spawn_blocking(move || crate::filesystem::rename_raw(&from, &to))
        .await
        .map_err(|_| IvyError::internal())?;
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            #[cfg(windows)]
            let cross_device = error.raw_os_error() == Some(17);
            #[cfg(unix)]
            let cross_device = error.raw_os_error() == Some(libc::EXDEV);
            if !cross_device {
                return Err(IvyError::internal());
            }
            preparation_transfer::copy_prepared_payload(source.clone(), destination, directory)
                .await?;
            // Only an actual EXDEV copy removes a source after publication. A
            // successful rename must not delete a subsequently recreated path.
            remove(source).await
        }
    }
}
impl State {
    fn pending(&mut self, value: BootstrapPackageComponent) {
        if let Some(prior) = self
            .pending
            .iter_mut()
            .find(|v| v.instance_id == value.instance_id)
        {
            *prior = value;
        } else {
            self.pending.push(value);
        }
    }
}
impl Inner {
    async fn result(&self, state: &mut State) -> IvyResult<PackageSyncResult> {
        let mut index = 0;
        while index < state.pending.len() {
            let component = &state.pending[index];
            if self
                .journal
                .installed(&component.instance_id)
                .await?
                .as_ref()
                .and_then(|v| v.get("candidateId"))
                == component.candidate.get("candidateId")
            {
                state.pending.remove(index);
            } else {
                index += 1;
            }
        }
        Ok(PackageSyncResult {
            bootstrap: state.pending.clone(),
        })
    }
    async fn installed_version(&self, instance: &str) -> IvyResult<Option<String>> {
        let Some(installed) = self.journal.installed(instance).await? else {
            return Ok(None);
        };
        match self
            .journal
            .manifest(&text(&installed, "candidateId")?)
            .await
        {
            Ok(manifest) => Ok(Some(text(&manifest, "version")?)),
            Err(error) if error.code == "invalid_arguments" => Ok(None),
            Err(error) => Err(error),
        }
    }
    async fn import_package(&self, entry: &IvyJson) -> IvyResult<IvyJson> {
        let host = self.journal.configuration().await?;
        let lock = ExecutorLock::acquire(
            config::path_value(host.required("stagingRoot")?)?
                .join("build-locks")
                .join(&text(entry, "buildId")?[7..]),
            false,
        )
        .await?;
        let result = self.import_locked(entry).await;
        if let Ok(candidate) = &result { crate::storage_retention::candidate_used(&host, candidate).await?; }
        lock.close().await?;
        result
    }
    async fn import_locked(&self, entry: &IvyJson) -> IvyResult<IvyJson> {
        let host = self.journal.config().await?;
        let artifacts = config::path_value(host.required("artifactRoot")?)?;
        let build = text(entry, "buildId")?;
        let destination = artifacts.join("candidates").join(&build[7..]);
        let candidate_at = |root: &std::path::Path| -> IvyResult<IvyJson> {
            Ok(IvyJson::object([
                ("candidateId", build.clone().into()),
                ("componentId", entry.required("componentId")?.clone()),
                ("buildId", build.clone().into()),
                (
                    "artifactRoot",
                    IvyJson::String(node_path::from_native(&root.join("artifact"))),
                ),
                (
                    "manifestPath",
                    IvyJson::String(node_path::from_native(&root.join("component.json"))),
                ),
                (
                    "archivePath",
                    IvyJson::String(node_path::from_native(&root.join("package.tar.gz"))),
                ),
                ("archiveHash", entry.required("archiveHash")?.clone()),
                ("createdAt", entry.required("publishedAt")?.clone()),
                ("platform", platform(self.node.as_ref())),
            ]))
        };
        match self.journal.candidate(&build).await {
            Ok(candidate) => {
                require(
                    candidate.get("archiveHash") == entry.get("archiveHash"),
                    "artifact_changed",
                    "Registered package archive differs from Hive.",
                )?;
                artifact::verify_candidate(
                    candidate.clone(),
                    artifacts.clone(),
                    self.node.as_ref(),
                )
                .await?;
                transfers::complete_existing(host.clone(), text(entry, "archiveHash")?, build.clone()).await?;
                return Ok(candidate);
            }
            Err(error) if error.code == "not_found" => (),
            Err(error) => return Err(error),
        }
        match tokio::fs::symlink_metadata(&destination).await {
            Ok(metadata) => {
                require(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "artifact_changed",
                    "Installed package candidate is invalid.",
                )?;
                let candidate = candidate_at(&destination)?;
                artifact::verify_candidate(
                    candidate.clone(),
                    artifacts.clone(),
                    self.node.as_ref(),
                )
                .await?;
                self.journal
                    .save_candidate(candidate.clone(), entry.required("manifest")?.clone())
                    .await?;
                transfers::complete_existing(host.clone(), text(entry, "archiveHash")?, build.clone()).await?;
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(IvyError::internal()),
        }
        let archive_hash = text(entry, "archiveHash")?;
        let transfer = transfers::Transfer::begin(host.clone(), archive_hash.clone(), build.clone()).await?;
        let archive = transfer.archive();
        let extraction = transfer.extraction();
        let incoming = artifacts.join(format!("incoming-package-{}", &archive_hash[7..]));
        remove(extraction.clone()).await?;
        remove(incoming.clone()).await?;
        tokio::fs::create_dir_all(archive.parent().ok_or_else(IvyError::internal)?)
            .await
            .map_err(|_| IvyError::internal())?;
        let result = async {
            let host = self.journal.config().await?;
            self.client
                .download(
                    text(&host, "publicBaseUrl")?,
                    self.credential.clone(),
                    entry.clone(),
                    archive.clone(),
                )
                .await?;
            let executables = host
                .required("executables")?
                .as_object()
                .ok_or_else(IvyError::internal)?
                .iter()
                .map(|(key, value)| Ok((key.to_utf8()?, config::path_value(value)?)))
                .collect::<IvyResult<BTreeMap<_, _>>>()?;
            package_receive::validate_package_archive(
                archive.clone(),
                archive_hash,
                entry.required("manifest")?.clone(),
                extraction.clone(),
                executables,
            )
            .await?;
            tokio::fs::create_dir_all(incoming.parent().ok_or_else(IvyError::internal)?)
                .await
                .map_err(|_| IvyError::internal())?;
            move_payload(extraction.clone(), incoming.clone(), true).await?;
            move_payload(archive.clone(), incoming.join("package.tar.gz"), false).await?;
            require(
                crate::paths::inside(&artifacts, &destination).map_err(|_| IvyError::internal())?
                    && crate::paths::inside(&artifacts, &incoming)
                        .map_err(|_| IvyError::internal())?,
                "target_conflict",
                "Package import leaves the host artifact root.",
            )?;
            tokio::fs::create_dir_all(destination.parent().ok_or_else(IvyError::internal)?)
                .await
                .map_err(|_| IvyError::internal())?;
            let from = incoming.clone();
            let to = destination.clone();
            tokio::task::spawn_blocking(move || crate::filesystem::rename_raw(&from, &to))
                .await
                .map_err(|_| IvyError::internal())?
                .map_err(|_| IvyError::internal())?;
            let candidate = candidate_at(&destination)?;
            artifact::verify_candidate(candidate.clone(), artifacts.clone(), self.node.as_ref())
                .await?;
            self.journal
                .save_candidate(candidate.clone(), entry.required("manifest")?.clone())
                .await?;
            Ok(candidate)
        }
        .await;
        if result.as_ref().err().is_none_or(|e: &IvyError| e.outcome != crate::Outcome::Unknown) {
            transfer.complete().await?;
        }
        result
    }
    async fn sync(&self, state: &mut State) -> IvyResult<PackageSyncResult> {
        let host = self.journal.configuration().await?;
        let Some(value) = self
            .client
            .catalog(
                text(&host, "publicBaseUrl")?,
                self.credential.clone(),
                state.revision,
            )
            .await?
        else {
            return self.result(state).await;
        };
        let index = package_validation::CatalogIndex::read(&value)?;
        let host = self.journal.configuration().await?;
        if let Some(executor) = instances(&host)?
            .iter()
            .find(|v| v.get("componentId") == Some(&"host-executor".into()))
            && self
                .journal
                .installed(&text(executor, "instanceId")?)
                .await?
                .is_some()
            && requires_bootstrap_replacement(executor)
        {
            let id = text(executor, "instanceId")?;
            let current = self.installed_version(&id).await?;
            let mut newest: Option<package_validation::PackageEntry<'_>> = None;
            for entry in index.packages.iter().filter(|entry| entry.component_id == "host-executor") {
                if let Some(current) = &current && json::compare_versions(&entry.version, current)? != Ordering::Greater { continue; }
                let entry = entry.clone().validate()?;
                if supported(entry.manifest, self.node.as_ref())?
                    && (newest.is_none() || json::compare_versions(&entry.version, &newest.as_ref().unwrap().version)? == Ordering::Greater) {
                    newest = Some(entry);
                }
            }
            if let Some(entry) = newest {
                let candidate = self.import_package(entry.value).await?;
                state.pending(BootstrapPackageComponent { instance_id: id, candidate, manifest: entry.manifest.clone() });
                state.revision = state.revision.max(index.revision);
                return self.result(state).await;
            }
        }
        let catalog = index.validate()?;
        // A vector preserves Map's first-insertion order, including replacements.
        let mut newest: Vec<package_validation::PackageEntry<'_>> = Vec::new();
        for entry in catalog.packages {
            if entry.manifest.get("kind") == Some(&"app".into()) || !supported(entry.manifest, self.node.as_ref())? { continue; }
            if let Some(previous) = newest
                .iter_mut()
                .find(|v| v.component_id == entry.component_id)
            {
                if json::compare_versions(&entry.version, &previous.version)? == Ordering::Greater {
                    *previous = entry;
                }
            } else {
                newest.push(entry);
            }
        }
        for entry in newest {
            let host = self.journal.configuration().await?;
            let component = IvyJson::from(entry.component_id.as_str());
            let instances = instances(&host)?
                .iter()
                .filter(|v| v.get("componentId") == Some(&component));
            let mut candidate: Option<IvyJson> = None;
            for instance in instances {
                let id = text(&instance, "instanceId")?;
                let installed = self.journal.installed(&id).await?;
                if installed.is_some()
                    && let Some(current) = self.installed_version(&id).await?
                    && json::compare_versions(&entry.version, &current)?
                        != Ordering::Greater
                {
                    continue;
                }
                if entry.manifest.get("kind") == Some(&"hive".into()) {
                    continue;
                }
                let candidate = match &candidate {
                    Some(value) => value.clone(),
                    None => {
                        let value = self.import_package(entry.value).await?;
                        candidate = Some(value.clone());
                        value
                    }
                };
                if installed.is_some() && requires_bootstrap_replacement(&instance) {
                    state.pending(BootstrapPackageComponent {
                        instance_id: id,
                        candidate,
                        manifest: entry.manifest.clone(),
                    });
                    continue;
                }
                let host = self.journal.configuration().await?;
                // HE02: instance identity is configured independently of component
                // identity. Keep the exact running executor build in replay IDs.
                let executor = instances_for_executor(&host)?;
                let executor_build = match executor {
                    Some(executor) => self
                        .journal
                        .installed(&text(executor, "instanceId")?)
                        .await?
                        .and_then(|value| value.get("buildId").cloned())
                        .unwrap_or(IvyJson::Null),
                    None => IvyJson::Null,
                };
                let hash = hash_json(&IvyJson::object([
                    ("instanceId", id.clone().into()),
                    ("version", entry.version.clone().into()),
                    ("buildId", IvyJson::String(entry.build_id.clone())),
                    ("configurationHash", hash_json(&host)?.into()),
                    ("executorBuildId", executor_build),
                ]))?;
                self.journal
                    .accept(
                        IvyJson::object([
                            ("action", "deploy".into()),
                            ("instanceId", id.into()),
                            ("operationId", format!("package-{}", &hash[7..47]).into()),
                            ("candidateId", candidate.required("candidateId")?.clone()),
                        ]),
                        None,
                    )
                    .await?;
            }
        }
        state.revision = state
            .revision
            .max(catalog.revision);
        self.result(state).await
    }
}
fn instances_for_executor(host: &IvyJson) -> IvyResult<Option<&IvyJson>> {
    Ok(instances(host)?
        .iter()
        .find(|v| v.get("componentId") == Some(&"host-executor".into())))
}
