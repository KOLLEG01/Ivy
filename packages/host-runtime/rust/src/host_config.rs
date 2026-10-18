//! Common local HostConfig admission. Validation retains JSON and creates only
//! the configured host/service directories, in the original validation order.
use crate::{
    IvyError, IvyJson, IvyResult, codex_home, config, layout, native_account,
    node_path::{self, Context, Style},
    paths,
};
use ivy_contracts::{JsString, core_schema::validate_host};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

/// Executing-account snapshot. No Debug implementation: environments can contain
/// credentials. Explicit contexts let callers keep one account/cwd generation.
pub struct HostContext {
    pub paths: Context,
    pub environment: BTreeMap<JsString, JsString>,
    pub account_home: JsString,
}
impl HostContext {
    pub fn current() -> IvyResult<Self> {
        let paths = Context::current().map_err(|_| IvyError::internal())?;
        let environment = native_account::environment();
        let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let os_home = match environment
            .get(&JsString::from(key))
            .filter(|v| !v.0.is_empty())
        {
            Some(home) => home.clone(),
            None => native_account::os_homedir().map_err(|_| IvyError::internal())?,
        };
        let account_home = layout::account_home(&environment, paths.style, &os_home)?;
        Ok(Self {
            paths,
            environment,
            account_home,
        })
    }
    fn home_context(&self) -> codex_home::HomeContext<'_> {
        codex_home::HomeContext {
            paths: &self.paths,
            environment: &self.environment,
            user_home: None,
            cwd: &self.paths.cwd,
            account_home: &self.account_home,
        }
    }
}

pub async fn host_config(path: impl Into<PathBuf>) -> IvyResult<IvyJson> {
    let path = path.into();
    tokio::task::spawn_blocking(move || {
        let input = config::read_json(
            &node_path::filesystem_path(&node_path::from_native(&path)),
            config::CONFIGURATION_BYTES,
        )
        .map_err(config::FileError::error)?;
        validate_host("HostConfigInput", &input)?;
        checked(input, &path, &HostContext::current()?)
    })
    .await
    .map_err(|_| IvyError::internal())?
}
pub async fn checked_host_config(
    input: IvyJson,
    configuration_path: impl Into<PathBuf>,
) -> IvyResult<IvyJson> {
    let path = configuration_path.into();
    tokio::task::spawn_blocking(move || {
        validate_host("HostConfigInput", &input)?;
        checked(input, &path, &HostContext::current()?)
    })
    .await
    .map_err(|_| IvyError::internal())?
}
/// Filesystem checks use this host's platform; the context must describe it.
pub async fn checked_host_config_with_context(
    input: IvyJson,
    configuration_path: PathBuf,
    context: HostContext,
) -> IvyResult<IvyJson> {
    tokio::task::spawn_blocking(move || {
        validate_host("HostConfigInput", &input)?;
        require(
            context.paths.style == Style::native(),
            "invalid_arguments",
            "Host configuration requires the local filesystem platform.",
        )?;
        checked(input, &configuration_path, &context)
    })
    .await
    .map_err(|_| IvyError::internal())?
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn text(value: &IvyJson) -> IvyResult<JsString> {
    value.as_js_string().cloned().ok_or_else(IvyError::internal)
}
fn field(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    text(value.required(key)?)
}
fn future(path: &JsString) -> IvyResult<JsString> {
    paths::future_path(path)
}
fn disjoint(paths: &Context, values: &[JsString]) -> bool {
    values.iter().enumerate().all(|(i, value)| {
        values
            .iter()
            .enumerate()
            .all(|(j, other)| i == j || !paths.inside(value, other))
    })
}
fn separate(paths: &Context, a: &JsString, b: &JsString) -> bool {
    !paths.inside(a, b) && !paths.inside(b, a)
}
fn instance_paths(config: &IvyJson, instance: &IvyJson) -> IvyResult<Vec<JsString>> {
    let paths = layout::service_paths(config, instance)?;
    paths
        .as_object()
        .ok_or_else(IvyError::internal)?
        .iter()
        .map(|(_, value)| text(value))
        .collect()
}

/// Only overlap coverage is used here, never storage keys or iteration order.
/// hostStorageAreas removes descendants already covered by earlier roots. The
/// unreduced union has the same bidirectional overlap coverage without locale
/// sorting; stable storage-area selection remains a separate layout operation.
fn permanent_roots(config: &IvyJson, context: &HostContext) -> IvyResult<Vec<JsString>> {
    let mut roots = vec![field(config, "runtimeRoot")?];
    for instance in config
        .required("instances")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        roots.extend(instance_paths(config, instance)?);
        if instance.get("componentId") == Some(&"agent-manager".into()) {
            roots.push(
                instance
                    .required("settings")?
                    .get("internalProjectRoot")
                    .and_then(IvyJson::as_js_string)
                    .cloned()
                    .unwrap_or_else(|| {
                        node_path::join(
                            context.paths.style,
                            &[
                                context.account_home.clone(),
                                ".ivy".into(),
                                "codex-projects".into(),
                            ],
                        )
                    }),
            );
        }
    }
    Ok(roots)
}
fn shared_home_paths(config: &IvyJson, context: &HostContext) -> IvyResult<Vec<JsString>> {
    let mut roots = permanent_roots(config, context)?;
    roots.extend([
        field(config, "artifactRoot")?,
        field(config, "stagingRoot")?,
    ]);
    let mut homes = Vec::new();
    for instance in config
        .required("instances")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        if instance.get("componentId") != Some(&"agent-manager".into()) {
            continue;
        }
        let settings = instance.required("settings")?;
        if !codex_home::uses_external_app_server_json(settings)?
            && codex_home::instance_owned_codex_home_json(
                settings,
                &field(&layout::service_paths(config, instance)?, "data")?,
                &context.home_context(),
            )?
            .is_some()
        {
            continue;
        }
        let home = codex_home::expected_server_home_json(settings, &context.home_context())?;
        let home = home.as_js_string().filter(|home| {
            node_path::is_absolute(context.paths.style, home)
                && roots
                    .iter()
                    .all(|root| separate(&context.paths, root, home))
        });
        require(
            home.is_some(),
            "target_conflict",
            "Shared Codex homes must not overlap owned host, service, project or software storage. Use explicit owned-stdio for an instance-owned home.",
        )?;
        homes.push(home.unwrap().clone());
    }
    Ok(homes)
}

fn checked(input: IvyJson, configuration_path: &Path, context: &HostContext) -> IvyResult<IvyJson> {
    let p = &context.paths;
    let configuration_path = p.resolve(&[node_path::from_native(configuration_path)]);
    let config = layout::resolve_host_configuration(
        &input,
        &configuration_path,
        &context.environment,
        p.style,
        &context.account_home,
    )?;
    validate_host("HostConfig", &config)?;
    let url = field(&config, "publicBaseUrl")?;
    ivy_sdk::rpc::base_url(&String::from_utf16_lossy(&url.0))?;
    if let Some(path) = config.get("configPath") {
        require(
            future(&text(path)?)? == future(&configuration_path)?,
            "target_conflict",
            "Central configPath must identify this actual host configuration file.",
        )?;
    }
    let instances = config
        .required("instances")?
        .as_array()
        .ok_or_else(IvyError::internal)?;
    let unique = |key: &str| {
        instances
            .iter()
            .map(|v| field(v, key))
            .collect::<IvyResult<BTreeSet<_>>>()
            .map(|v| v.len() == instances.len())
    };
    require(
        unique("instanceId")? && unique("serviceNodeId")?,
        "invalid_arguments",
        "Host instance and Service Node identities must be unique.",
    )?;
    let roots = [
        field(&config, "runtimeRoot")?,
        field(&config, "artifactRoot")?,
        field(&config, "stagingRoot")?,
    ];
    require(
        roots.iter().all(|v| node_path::is_absolute(p.style, v)) && disjoint(p, &roots),
        "invalid_arguments",
        "Host runtime, artifact and staging roots must be disjoint absolute directories.",
    )?;
    require(
        !p.inside(&roots[1], &configuration_path) && !p.inside(&roots[2], &configuration_path),
        "invalid_arguments",
        "Private host configuration cannot live inside artifacts or source staging.",
    )?;
    let services: Vec<_> = instances
        .iter()
        .map(|i| instance_paths(&config, i))
        .collect::<IvyResult<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    require(
        services.iter().all(|v| node_path::is_absolute(p.style, v)) && disjoint(p, &services),
        "target_conflict",
        "Service data, work and logs collide; configure explicitly separate paths for repeated service instances.",
    )?;
    let modern = config
        .get("servicesRoot")
        .is_some_and(|v| v.as_js_string().is_some_and(|v| !v.0.is_empty()));
    let excluded = if modern { &roots[..] } else { &roots[1..] };
    require(
        services.iter().all(|v| {
            excluded.iter().all(|r| separate(p, r, v)) && !p.inside(v, &configuration_path)
        }),
        "target_conflict",
        "Service storage must not overlap software, staging, host state or central configuration.",
    )?;
    for root in &roots {
        fs::create_dir_all(node_path::filesystem_path(root)).map_err(|_| IvyError::internal())?;
    }
    let actual_roots: Vec<_> = roots
        .iter()
        .map(|r| {
            paths::realpath(&node_path::to_native(r))
                .map(|v| node_path::from_native(&v))
                .map_err(|_| IvyError::internal())
        })
        .collect::<IvyResult<_>>()?;
    require(
        disjoint(p, &actual_roots),
        "invalid_arguments",
        "Host roots overlap through filesystem links.",
    )?;
    let actual_services = services.iter().map(future).collect::<IvyResult<Vec<_>>>()?;
    require(
        disjoint(p, &actual_services)
            && actual_services.iter().all(|v| {
                actual_roots[if modern { 0 } else { 1 }..]
                    .iter()
                    .all(|r| separate(p, r, v))
            }),
        "target_conflict",
        "Service storage overlaps through filesystem links.",
    )?;
    let mut internal = Vec::new();
    let mut seen = BTreeSet::new();
    for instance in instances
        .iter()
        .filter(|v| v.get("componentId") == Some(&"agent-manager".into()))
    {
        if let Some(root) = instance
            .required("settings")?
            .get("internalProjectRoot")
            .and_then(IvyJson::as_js_string)
            .filter(|v| seen.insert((*v).clone()))
        {
            require(
                node_path::is_absolute(p.style, root),
                "invalid_arguments",
                "Internal project root must be absolute.",
            )?;
            internal.push(future(root)?);
        }
    }
    let shared = shared_home_paths(&config, context)?
        .iter()
        .map(future)
        .collect::<IvyResult<Vec<_>>>()?;
    let configuration = p.resolve(&[config
        .get("configPath")
        .map(text)
        .transpose()?
        .unwrap_or_else(|| configuration_path.clone())]);
    require(
        disjoint(p, &internal)
            && internal.iter().all(|v| {
                actual_roots
                    .iter()
                    .chain(&actual_services)
                    .chain(&shared)
                    .all(|r| separate(p, r, v))
                    && !p.inside(v, &configuration)
            }),
        "target_conflict",
        "Internal project storage overlaps service state, software, shared Codex homes or configuration.",
    )?;
    for instance in instances
        .iter()
        .filter(|v| v.get("componentId") == Some(&"agent-manager".into()))
    {
        if let Some(root) = instance.required("settings")?.get("projectRoot") {
            let root = root
                .as_js_string()
                .filter(|v| node_path::is_absolute(p.style, v));
            require(
                root.is_some(),
                "invalid_arguments",
                "Normal project root must be absolute.",
            )?;
            let normal = future(root.unwrap())?;
            require(
                actual_roots
                    .iter()
                    .chain(&actual_services)
                    .chain(&shared)
                    .chain(&internal)
                    .all(|r| !p.inside(r, &normal)),
                "target_conflict",
                "New normal projects must not be allocated inside private service/internal/Codex storage or executable artifacts.",
            )?;
        }
    }
    for directory in &services {
        fs::create_dir_all(node_path::filesystem_path(directory))
            .map_err(|_| IvyError::internal())?;
    }
    for directory in roots.iter().chain(&services) {
        writable(&node_path::filesystem_path(directory)).map_err(|_| IvyError::internal())?;
    }
    for home in shared_home_paths(&config, context)? {
        let actual = future(&home)?;
        require(
            actual_roots
                .iter()
                .chain(&actual_services)
                .all(|r| separate(p, r, &actual)),
            "target_conflict",
            "Shared Codex homes cannot overlap owned runtime, service, artifact or staging storage through filesystem links.",
        )?;
    }
    let executables = config
        .required("executables")?
        .as_object()
        .ok_or_else(IvyError::internal)?;
    for (_, value) in executables {
        require(
            node_path::is_absolute(p.style, &text(value)?),
            "invalid_arguments",
            "Host runtime executable mappings require absolute paths.",
        )?;
    }
    for instance in instances {
        require(
            instance.get("engine") != Some(&"docker".into())
                || cfg!(target_os = "linux") && instance.get("docker").is_some(),
            "invalid_arguments",
            "Docker instances require an explicit Linux container configuration.",
        )?;
        let id = field(instance, "instanceId")?;
        let component = field(instance, "componentId")?;
        let alphanumeric =
            |c: &u16| (48..=57).contains(c) || (65..=90).contains(c) || (97..=122).contains(c);
        require(
            !id.0.is_empty()
                && id.0.len() <= 128
                && alphanumeric(&id.0[0])
                && id.0.iter().all(|c| alphanumeric(c) || [45, 95].contains(c))
                && component.0.first().is_some_and(|c| (97..=122).contains(c))
                && component
                    .0
                    .iter()
                    .all(|c| (97..=122).contains(c) || (48..=57).contains(c) || *c == 45),
            "invalid_arguments",
            "Instance/component IDs must be safe local names.",
        )?;
    }
    Ok(config)
}

#[cfg(unix)]
fn writable(path: &Path) -> std::io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    if unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
#[cfg(windows)]
fn writable(path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY, GetFileAttributesW,
        INVALID_FILE_ATTRIBUTES,
    };
    let path: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
    let attributes = unsafe { GetFileAttributesW(path.as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES {
        Err(std::io::Error::last_os_error())
    } else if attributes & FILE_ATTRIBUTE_READONLY != 0
        && attributes & FILE_ATTRIBUTE_DIRECTORY == 0
    {
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    } else {
        Ok(())
    }
}
