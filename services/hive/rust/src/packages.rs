//! Package grants, private upload staging and authoritative catalog file collection.
use crate::{store::sql_error, worker_client::RequestOptions, worker_runtime::StorageClient};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::digest,
    json::{canonical, compare_versions, parse},
};
use ivy_host_runtime::{
    package_archive::{
        validate_package_archive, validate_package_authorization, validate_package_catalog,
    },
    paths::absolute,
    streaming_hash::file_hash,
};
use sha2::{Digest, Sha256};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    time::UNIX_EPOCH,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::oneshot,
};

pub type PackageRequest = Pin<Box<dyn Future<Output = IvyResult<IvyJson>> + Send>>;
/// Requests post eagerly using the existing storage-worker protocol. Dropping
/// their result must not cancel the posted package action.
pub trait PackageStorage: Send + Sync {
    fn request(&self, request: IvyJson) -> PackageRequest;
}
impl PackageStorage for StorageClient {
    fn request(&self, request: IvyJson) -> PackageRequest {
        let reply = self.request(request, RequestOptions::default());
        Box::pin(async move { reply.await.map_err(sql_error)? })
    }
}
/// The kernel binds its existing UTC clock and JavaScript-compatible ISO formatter.
pub trait PackageClock: Send + Sync {
    fn now(&self) -> f64;
    fn iso_date(&self, milliseconds: f64) -> IvyResult<JsString>;
}
pub struct PackageUiInstallation {
    pub manifest: IvyJson,
    pub artifact_root: PathBuf,
    pub publisher_principal_id: JsString,
    pub build_id: JsString,
}
pub type PackageInstallation = Pin<Box<dyn Future<Output = IvyResult<()>> + Send>>;
pub trait PackageUiInstaller: Send + Sync {
    fn install(&self, input: PackageUiInstallation) -> PackageInstallation;
}
#[derive(Default)]
pub struct PackageOptions {
    pub executables: BTreeMap<String, PathBuf>,
    pub install_ui: Option<Arc<dyn PackageUiInstaller>>,
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn string<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    field(value, key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn catalog_request(after: IvyJson, include_uis: bool) -> IvyJson {
    IvyJson::object([
        ("action", "package.catalog".into()),
        ("after", after),
        ("includeUis", include_uis.into()),
    ])
}
fn component(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}
fn version(name: &str) -> bool {
    let parts: Vec<_> = name.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes().all(|c| c.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
        })
}
fn incoming_name(name: &str) -> bool {
    let Some(id) = name
        .strip_suffix(".tar.gz")
        .or_else(|| name.strip_suffix("-extract"))
    else {
        return false;
    };
    id.len() == 36
        && id.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_digit() || (b'a'..=b'f').contains(&c)
            }
        })
}
fn entries(path: &Path) -> IvyResult<Vec<fs::DirEntry>> {
    let values = match fs::read_dir(path) {
        Ok(values) => values,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(sql_error(e)),
    };
    let mut values = values.collect::<io::Result<Vec<_>>>().map_err(sql_error)?;
    // Node scandir's byte ordering also determines which filesystem guard fails first.
    values.sort_by_key(|v| v.file_name().to_string_lossy().as_bytes().to_vec());
    Ok(values)
}
fn info(path: &Path) -> IvyResult<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(info) => Ok(Some(info)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(sql_error(e)),
    }
}
fn contained(root: &Path, path: &Path) -> bool {
    let mut prefix = root.as_os_str().to_os_string();
    prefix.push(std::path::MAIN_SEPARATOR_STR);
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .collect::<Vec<_>>()
            .starts_with(&prefix.encode_wide().collect::<Vec<_>>())
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().starts_with(prefix.as_bytes())
    }
}
fn remove(path: &Path, recursive: bool) -> IvyResult<()> {
    let Some(info) = info(path)? else {
        return Ok(());
    };
    let result = if info.is_dir() && recursive {
        fs::remove_dir_all(path)
    } else {
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileTypeExt;
            if info.file_type().is_symlink_dir() {
                return fs::remove_dir(path).map_err(sql_error);
            }
        }
        fs::remove_file(path)
    };
    match result {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other.map_err(sql_error),
    }
}

struct Grant {
    input: IvyJson,
    upload_id: JsString,
    token_digest: String,
    principal_id: JsString,
    expires_at: f64,
}
/// This capability can only be constructed by consuming a valid one-time grant.
pub struct AuthorizedUpload(Grant);
impl AuthorizedUpload {
    pub fn authorization(&self) -> &IvyJson {
        &self.0.input
    }
    pub fn upload_id(&self) -> &JsString {
        &self.0.upload_id
    }
    pub fn principal_id(&self) -> &JsString {
        &self.0.principal_id
    }
}
/// Private staged bytes, retained until archive admission and publication finish.
/// Dropping the staging owner performs the original upload's best-effort cleanup.
pub struct StagedPackageUpload {
    grant: AuthorizedUpload,
    archive: PathBuf,
    extraction: PathBuf,
    cleanup_pending: bool,
}
impl StagedPackageUpload {
    pub fn authorization(&self) -> &IvyJson {
        self.grant.authorization()
    }
    pub fn principal_id(&self) -> &JsString {
        self.grant.principal_id()
    }
    pub fn archive(&self) -> &Path {
        &self.archive
    }
    pub fn extraction(&self) -> &Path {
        &self.extraction
    }
    pub async fn cleanup(mut self) {
        let archive = self.archive.clone();
        let extraction = self.extraction.clone();
        self.cleanup_pending = false;
        let _ = tokio::task::spawn_blocking(move || {
            let _ = remove(&archive, false);
            let _ = remove(&extraction, true);
        })
        .await;
    }
}
impl Drop for StagedPackageUpload {
    fn drop(&mut self) {
        if self.cleanup_pending {
            let _ = remove(&self.archive, false);
            let _ = remove(&self.extraction, true);
        }
    }
}
#[derive(Default)]
struct CommitQueue(Mutex<Option<oneshot::Receiver<()>>>);
struct Turn {
    previous: Option<oneshot::Receiver<()>>,
    release: Option<oneshot::Sender<()>>,
}
impl CommitQueue {
    fn reserve(&self) -> Turn {
        let (release, next) = oneshot::channel();
        Turn {
            previous: self.0.lock().unwrap().replace(next),
            release: Some(release),
        }
    }
}
impl Turn {
    async fn wait(&mut self) {
        if let Some(previous) = self.previous.take() {
            let _ = previous.await;
        }
    }
}
impl Drop for Turn {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}
struct State {
    root: PathBuf,
    base_path: JsString,
    clock: Arc<dyn PackageClock>,
    worker: Option<Arc<dyn PackageStorage>>,
    grants: Mutex<HashMap<JsString, Grant>>,
    commits: CommitQueue,
    options: PackageOptions,
}
#[derive(Clone)]
pub struct PackageRegistry(Arc<State>);
impl PackageRegistry {
    pub fn new(
        root: impl Into<PathBuf>,
        base_path: JsString,
        clock: Arc<dyn PackageClock>,
        worker: Option<Arc<dyn PackageStorage>>,
        options: PackageOptions,
    ) -> Self {
        Self(Arc::new(State {
            root: root.into(),
            base_path,
            clock,
            worker,
            grants: Mutex::new(HashMap::new()),
            commits: CommitQueue::default(),
            options,
        }))
    }
    pub fn root(&self) -> &Path {
        &self.0.root
    }
    fn worker(&self) -> IvyResult<&Arc<dyn PackageStorage>> {
        self.0.worker.as_ref().ok_or_else(|| {
            IvyError::new(
                "service_unavailable",
                "Hive package storage worker is unavailable.",
            )
        })
    }
    pub fn catalog_snapshot(&self, after: &IvyJson, include_uis: bool) -> PackageRequest {
        match self.worker() {
            Ok(worker) => worker.request(catalog_request(after.clone(), include_uis)),
            Err(error) => Box::pin(std::future::ready(Err(error))),
        }
    }
    pub async fn catalog(&self, after: &IvyJson, include_uis: bool) -> IvyResult<IvyJson> {
        Ok(field(&self.catalog_snapshot(after, include_uis).await?, "catalog")?.clone())
    }
    pub async fn initialize(&self) -> IvyResult<()> {
        let this = self.clone();
        tokio::spawn(async move { this.initialize_owned().await })
            .await
            .map_err(sql_error)?
    }
    async fn initialize_owned(&self) -> IvyResult<()> {
        let worker = self.worker()?;
        tokio::fs::create_dir_all(&self.0.root)
            .await
            .map_err(sql_error)?;
        match worker
            .request(catalog_request(IvyJson::Number(0.0), true))
            .await
        {
            Ok(_) => {}
            Err(e) if e.code == "service_unavailable" => {
                let catalog = match tokio::fs::read(self.0.root.join("catalog.json")).await {
                    Ok(bytes) => validate_package_catalog(
                        &parse(&String::from_utf8_lossy(&bytes), usize::MAX).map_err(sql_error)?,
                    )?,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => IvyJson::object([
                        ("schemaVersion", IvyJson::Number(1.0)),
                        ("revision", IvyJson::Number(0.0)),
                        ("packages", IvyJson::Array(vec![])),
                    ]),
                    Err(e) => return Err(sql_error(e)),
                };
                worker
                    .request(IvyJson::object([
                        ("action", "package.seed".into()),
                        ("catalog", catalog),
                    ]))
                    .await?;
            }
            Err(e) => return Err(e),
        }
        worker
            .request(IvyJson::object([("action", "package.prune".into())]))
            .await?;
        self.collect_published().await?;
        self.collect_incoming_owned().await
    }
    fn collect_grants(&self) {
        let now = self.0.clock.now();
        self.0
            .grants
            .lock()
            .unwrap()
            .retain(|_, grant| !(grant.expires_at <= now));
    }
    pub fn authorize(&self, principal_id: &JsString, input: &IvyJson) -> PackageRequest {
        self.collect_grants();
        let request = match validate_package_authorization(input) {
            Ok(request) => request,
            Err(error) => return Box::pin(std::future::ready(Err(error))),
        };
        let catalog = self.catalog_snapshot(&IvyJson::Number(0.0), true);
        let this = self.clone();
        let principal_id = principal_id.clone();
        let work =
            tokio::spawn(async move { this.authorize_owned(principal_id, request, catalog).await });
        Box::pin(async move { work.await.map_err(sql_error)? })
    }
    async fn authorize_owned(
        &self,
        principal_id: JsString,
        request: IvyJson,
        catalog: PackageRequest,
    ) -> IvyResult<IvyJson> {
        let snapshot = catalog.await?;
        let catalog = field(&snapshot, "catalog")?;
        let latest = latest(&catalog, string(&request, "componentId")?)?;
        if let Some(latest) = latest {
            if compare_versions(
                &field(&request, "version")?.as_str()?,
                &field(latest, "version")?.as_str()?,
            )? != Ordering::Greater
            {
                require(
                    request.get("version") == latest.get("version")
                        && request.get("archiveHash") == latest.get("archiveHash")
                        && request.get("buildId") == latest.get("buildId")
                        && canonical(field(&request, "manifest")?, usize::MAX)?
                            == canonical(field(latest, "manifest")?, usize::MAX)?,
                    "version_conflict",
                    "Published package versions are immutable and must increase.",
                )?;
                return Ok(IvyJson::object([
                    ("alreadyPublished", true.into()),
                    ("entry", latest.clone()),
                ]));
            }
        }
        let mut grants = self.0.grants.lock().unwrap();
        require(
            grants.len() < 128,
            "capacity_exceeded",
            "Too many package uploads are pending.",
        )?;
        let upload_id: JsString = uuid::Uuid::new_v4().to_string().into();
        let mut random = [0; 32];
        getrandom::fill(&mut random).map_err(sql_error)?;
        let token = format!("ivypu1.{}", URL_SAFE_NO_PAD.encode(random));
        let expires_at = self.0.clock.now() + 300_000.0;
        grants.insert(
            upload_id.clone(),
            Grant {
                input: request,
                upload_id: upload_id.clone(),
                token_digest: digest(&token),
                principal_id,
                expires_at,
            },
        );
        let mut upload_path = self.0.base_path.clone();
        upload_path
            .0
            .extend(format!("/api/v1/packages/uploads/{}", text(&upload_id)).encode_utf16());
        Ok(IvyJson::object([
            ("alreadyPublished", false.into()),
            ("uploadId", IvyJson::String(upload_id)),
            ("uploadToken", token.into()),
            ("uploadPath", IvyJson::String(upload_path)),
            (
                "expiresAt",
                IvyJson::String(self.0.clock.iso_date(expires_at)?),
            ),
        ]))
    }
    /// Run before reading the request body. Malformed headers consume a valid grant.
    pub fn admit_upload(
        &self,
        upload_id: &JsString,
        headers: &IvyJson,
    ) -> IvyResult<AuthorizedUpload> {
        self.collect_grants();
        let token = headers
            .get("authorization")
            .and_then(IvyJson::as_js_string)
            .map(text);
        let valid = token
            .as_deref()
            .and_then(|s| s.strip_prefix("Bearer ivypu1."))
            .is_some_and(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
            });
        require(
            valid,
            "unauthenticated",
            "Package upload requires its one-time token.",
        )?;
        let token = token.unwrap();
        let mut grants = self.0.grants.lock().unwrap();
        require(
            grants.get(upload_id).is_some_and(|g| {
                g.expires_at > self.0.clock.now() && g.token_digest == digest(&token[7..])
            }),
            "unauthenticated",
            "Package upload token is invalid or expired.",
        )?;
        let grant = grants.remove(upload_id).unwrap();
        drop(grants);
        require(
            headers.get("content-encoding").is_none(),
            "invalid_arguments",
            "Package upload content encoding is unsupported.",
        )?;
        let declared = headers
            .get("content-length")
            .and_then(IvyJson::as_js_string);
        require(
            declared.is_some_and(|s| {
                !s.0.is_empty()
                    && s.0.iter().all(|c| (b'0' as u16..=b'9' as u16).contains(c))
                    && s.to_utf8()
                        .ok()
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(|n| n as f64)
                        == grant.input.get("bytes").and_then(IvyJson::as_f64)
            }),
            "invalid_arguments",
            "Package upload length differs from its grant.",
        )?;
        Ok(AuthorizedUpload(grant))
    }
    /// Body ingress only; the Host receiver must admit this archive before publication.
    /// The HTTP adapter supplies the original five-minute socket idle timeout.
    pub async fn stage_upload<R: AsyncRead + Unpin + Send + 'static>(
        &self,
        grant: AuthorizedUpload,
        body: R,
    ) -> IvyResult<StagedPackageUpload> {
        let root = self.0.root.clone();
        tokio::spawn(async move { stage(root, grant, body).await })
            .await
            .map_err(sql_error)?
    }
    pub async fn upload<R: AsyncRead + Unpin + Send + 'static>(
        &self,
        upload_id: &JsString,
        headers: &IvyJson,
        body: R,
    ) -> IvyResult<IvyJson> {
        let grant = self.admit_upload(upload_id, headers)?;
        let this = self.clone();
        tokio::spawn(async move {
            let staged = stage(this.0.root.clone(), grant, body).await?;
            let result = async {
                validate_package_archive(
                    staged.archive.clone(),
                    text(string(staged.authorization(), "archiveHash")?),
                    field(staged.authorization(), "manifest")?.clone(),
                    staged.extraction.clone(),
                    this.0.options.executables.clone(),
                )
                .await?;
                let mut turn = this.0.commits.reserve();
                turn.wait().await;
                let result = this.commit_upload(&staged).await;
                if result.is_err() {
                    let _ = this.collect_published().await;
                }
                result
            }
            .await;
            staged.cleanup().await;
            result
        })
        .await
        .map_err(sql_error)?
    }
    async fn commit_upload(&self, staged: &StagedPackageUpload) -> IvyResult<IvyJson> {
        let grant = staged.authorization();
        let catalog = self.catalog(&IvyJson::Number(0.0), true).await?;
        if let Some(latest) = latest(&catalog, string(grant, "componentId")?)? {
            if compare_versions(
                &field(grant, "version")?.as_str()?,
                &field(latest, "version")?.as_str()?,
            )? != Ordering::Greater
            {
                require(
                    grant.get("version") == latest.get("version")
                        && grant.get("archiveHash") == latest.get("archiveHash")
                        && grant.get("buildId") == latest.get("buildId"),
                    "version_conflict",
                    "Published package versions are immutable and must increase.",
                )?;
                return Ok(latest.clone());
            }
        }
        let manifest = field(grant, "manifest")?;
        if manifest.get("kind") == Some(&"app".into()) {
            if let Some(installer) = &self.0.options.install_ui {
                installer
                    .install(PackageUiInstallation {
                        manifest: manifest.clone(),
                        artifact_root: staged.extraction.join("artifact"),
                        publisher_principal_id: staged.principal_id().clone(),
                        build_id: string(grant, "buildId")?.clone(),
                    })
                    .await?;
            }
            require(
                self.0.options.install_ui.is_some(),
                "service_unavailable",
                "Hive has no app package installer.",
            )?;
        }
        let component = self
            .0
            .root
            .join("artifacts")
            .join(text(string(grant, "componentId")?));
        let target = component
            .join(text(string(grant, "version")?))
            .join(format!(
                "{}.tar.gz",
                &text(string(grant, "archiveHash")?)[7..]
            ));
        tokio::fs::create_dir_all(target.parent().ok_or_else(IvyError::internal)?)
            .await
            .map_err(sql_error)?;
        let marker = component.join(".package");
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
            .await
        {
            Ok(file) => drop(file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(sql_error(error)),
        }
        let metadata = tokio::fs::symlink_metadata(marker)
            .await
            .map_err(sql_error)?;
        require(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "storage_invalid",
            "Package artifact marker is invalid.",
        )?;
        let retained = match tokio::fs::symlink_metadata(&target).await {
            Ok(info) => Some(info),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(sql_error(error)),
        };
        if let Some(retained) = retained {
            require(
                retained.is_file()
                    && !retained.file_type().is_symlink()
                    && Some(retained.len() as f64) == grant.get("bytes").and_then(IvyJson::as_f64)
                    && file_hash(&target).await? == text(string(grant, "archiveHash")?),
                "artifact_changed",
                "Retained package archive differs from the authorized upload.",
            )?;
        } else {
            tokio::fs::rename(&staged.archive, target)
                .await
                .map_err(sql_error)?;
        }
        let result = self
            .worker()?
            .request(IvyJson::object([
                ("action", "package.publish".into()),
                ("input", grant.clone()),
                (
                    "publisherPrincipalId",
                    IvyJson::String(staged.principal_id().clone()),
                ),
            ]))
            .await?;
        self.collect_published().await?;
        Ok(result)
    }
    pub async fn artifact(
        &self,
        component_id: &JsString,
        version: &JsString,
    ) -> IvyResult<(PathBuf, IvyJson)> {
        let catalog = self.catalog(&IvyJson::Number(0.0), true).await?;
        let entry = field(&catalog, "packages")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .find(|e| {
                e.get("componentId").and_then(IvyJson::as_js_string) == Some(component_id)
                    && e.get("version").and_then(IvyJson::as_js_string) == Some(version)
            })
            .ok_or_else(|| IvyError::new("not_found", "Package version was not found."))?;
        let path = self
            .0
            .root
            .join("artifacts")
            .join(text(component_id))
            .join(text(version))
            .join(format!(
                "{}.tar.gz",
                &text(string(entry, "archiveHash")?)[7..]
            ));
        let info = tokio::fs::symlink_metadata(&path)
            .await
            .map_err(sql_error)?;
        require(
            info.is_file()
                && !info.file_type().is_symlink()
                && Some(info.len() as f64) == entry.get("bytes").and_then(IvyJson::as_f64),
            "artifact_changed",
            "Published package artifact is unavailable.",
        )?;
        Ok((path, entry.clone()))
    }
    pub async fn collect_artifacts(&self) -> IvyResult<()> {
        let this = self.clone();
        let mut turn = self.0.commits.reserve();
        // A waiting caller disappearing must not release the chain or cancel pruning.
        tokio::spawn(async move {
            turn.wait().await;
            this.0
                .worker
                .as_ref()
                .ok_or_else(IvyError::internal)?
                .request(IvyJson::object([("action", "package.prune".into())]))
                .await?;
            this.collect_published().await?;
            this.collect_incoming_owned().await
        })
        .await
        .map_err(sql_error)?
    }
    async fn collect_published(&self) -> IvyResult<()> {
        let catalog = self.catalog(&IvyJson::Number(0.0), true).await?;
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.collect_published_files(&catalog))
            .await
            .map_err(sql_error)?
    }
    fn collect_published_files(&self, catalog: &IvyJson) -> IvyResult<()> {
        let root = absolute(&self.0.root.join("artifacts")).map_err(sql_error)?;
        let mut components: Vec<(JsString, HashSet<JsString>)> = vec![];
        let mut positions = HashMap::new();
        for entry in field(catalog, "packages")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            let id = string(entry, "componentId")?;
            let index = *positions.entry(id.clone()).or_insert_with(|| {
                components.push((id.clone(), HashSet::new()));
                components.len() - 1
            });
            components[index]
                .1
                .insert(string(entry, "version")?.clone());
        }
        for entry in entries(&root)? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type().map_err(sql_error)?.is_dir() || !component(&name) {
                continue;
            }
            for marker in [".app", ".package"] {
                if let Some(info) = info(&root.join(&name).join(marker))? {
                    require(
                        info.is_file() && !info.file_type().is_symlink(),
                        "storage_invalid",
                        "Package artifact marker is invalid.",
                    )?;
                    positions.entry(name.as_str().into()).or_insert_with(|| {
                        components.push((name.as_str().into(), HashSet::new()));
                        components.len() - 1
                    });
                }
            }
        }
        for (id, versions) in components {
            let directory = absolute(&root.join(text(&id))).map_err(sql_error)?;
            require(
                contained(&root, &directory),
                "storage_invalid",
                "Package artifact directory is invalid.",
            )?;
            let Some(info) = info(&directory)? else {
                continue;
            };
            require(
                info.is_dir() && !info.file_type().is_symlink(),
                "storage_invalid",
                "Package artifact directory is invalid.",
            )?;
            for entry in entries(&directory)? {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !entry.file_type().map_err(sql_error)?.is_dir()
                    || versions.contains(&name.as_str().into())
                    || !version(&name)
                {
                    continue;
                }
                let target = absolute(&directory.join(&name)).map_err(sql_error)?;
                require(
                    contained(&directory, &target),
                    "storage_invalid",
                    "Package artifact path is invalid.",
                )?;
                remove(&target, true)?;
            }
            if versions.is_empty()
                && entries(&directory)?
                    .iter()
                    .all(|v| matches!(v.file_name().to_str(), Some(".app" | ".package")))
            {
                remove(&directory.join(".app"), false)?;
                remove(&directory.join(".package"), false)?;
                fs::remove_dir(directory).map_err(sql_error)?;
            }
        }
        Ok(())
    }
    async fn collect_incoming_owned(&self) -> IvyResult<()> {
        let this = self.clone();
        let now = self.0.clock.now();
        tokio::task::spawn_blocking(move || this.collect_incoming(now))
            .await
            .map_err(sql_error)?
    }
    fn collect_incoming(&self, now: f64) -> IvyResult<()> {
        let directory = absolute(&self.0.root.join("incoming")).map_err(sql_error)?;
        for entry in entries(&directory)? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !incoming_name(&name) {
                continue;
            }
            let kind = entry.file_type().map_err(sql_error)?;
            if !kind.is_file() && !kind.is_dir() {
                continue;
            }
            let target = absolute(&directory.join(&name)).map_err(sql_error)?;
            require(
                contained(&directory, &target),
                "storage_invalid",
                "Incoming package path is invalid.",
            )?;
            let modified = fs::symlink_metadata(&target)
                .map_err(sql_error)?
                .modified()
                .map_err(sql_error)?;
            let ms = match modified.duration_since(UNIX_EPOCH) {
                Ok(d) => d.as_secs() as f64 * 1000.0 + d.subsec_nanos() as f64 / 1_000_000.0,
                Err(e) => {
                    -(e.duration().as_secs() as f64 * 1000.0
                        + e.duration().subsec_nanos() as f64 / 1_000_000.0)
                }
            };
            if ms > now - 86_400_000.0 {
                continue;
            }
            remove(&target, kind.is_dir())?;
        }
        Ok(())
    }
}
fn latest<'a>(catalog: &'a IvyJson, component: &JsString) -> IvyResult<Option<&'a IvyJson>> {
    let mut latest: Option<&IvyJson> = None;
    for entry in field(catalog, "packages")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        if entry.get("componentId").and_then(IvyJson::as_js_string) != Some(component) {
            continue;
        }
        if latest.is_none()
            || compare_versions(
                &field(entry, "version")?.as_str()?,
                &field(latest.unwrap(), "version")?.as_str()?,
            )? == Ordering::Greater
        {
            latest = Some(entry);
        }
    }
    Ok(latest)
}
async fn stage<R: AsyncRead + Unpin>(
    root: PathBuf,
    grant: AuthorizedUpload,
    mut body: R,
) -> IvyResult<StagedPackageUpload> {
    let incoming = root.join("incoming");
    tokio::fs::create_dir_all(&incoming)
        .await
        .map_err(sql_error)?;
    let id = text(grant.upload_id());
    let staged = StagedPackageUpload {
        grant,
        archive: incoming.join(format!("{id}.tar.gz")),
        extraction: incoming.join(format!("{id}-extract")),
        cleanup_pending: true,
    };
    let expected = field(staged.authorization(), "bytes")?
        .as_f64()
        .ok_or_else(IvyError::internal)? as u64;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged.archive)
        .await
        .map_err(sql_error)?;
    let mut buffer = [0; 64 * 1024];
    let mut bytes = 0;
    let mut hash = Sha256::new();
    loop {
        let length = body.read(&mut buffer).await.map_err(sql_error)?;
        if length == 0 {
            break;
        }
        bytes += length as u64;
        require(
            bytes <= expected,
            "content_too_large",
            "Package upload exceeds its authorized size.",
        )?;
        hash.update(&buffer[..length]);
        file.write_all(&buffer[..length]).await.map_err(sql_error)?;
    }
    file.flush().await.map_err(sql_error)?;
    drop(file);
    let mut actual = String::from("sha256:");
    for byte in hash.finalize() {
        use std::fmt::Write;
        write!(actual, "{byte:02x}").unwrap();
    }
    require(
        bytes == expected
            && Some(&JsString::from(actual))
                == staged
                    .authorization()
                    .get("archiveHash")
                    .and_then(IvyJson::as_js_string),
        "artifact_changed",
        "Package upload checksum differs from its grant.",
    )?;
    Ok(staged)
}
