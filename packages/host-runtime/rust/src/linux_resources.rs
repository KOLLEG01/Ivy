//! Immutable systemd budget identity and the actual kernel membership boundary.
use crate::{
    IvyError, IvyJson, IvyResult,
    linux_host::{LinuxHost, NativeLinuxHost},
};
use ivy_contracts::{JsString, core_schema::validate_host, hash::hash_json};
use std::{io, path::PathBuf, sync::Arc};
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> {
    value.map_err(|_| IvyError::internal())
}
fn string(value: &IvyJson) -> IvyResult<JsString> {
    value.as_js_string().cloned().ok_or_else(IvyError::internal)
}
fn truthy(value: &IvyJson) -> bool {
    match value {
        IvyJson::Null => false,
        IvyJson::Bool(v) => *v,
        IvyJson::Number(v) => *v != 0.0 && !v.is_nan(),
        IvyJson::String(v) => !v.0.is_empty(),
        _ => true,
    }
}
fn budget_text(value: &IvyJson) -> IvyResult<JsString> {
    value.as_f64().filter(|value| value.is_finite() && value.fract() == 0.0 && (0.0..=9_007_199_254_740_991.0).contains(value))
        .map(|value| value.to_string().into()).ok_or_else(IvyError::internal)
}
pub fn installation_identity(config: &IvyJson) -> IvyResult<String> {
    let value = match config.get("restoredFrom").filter(|value| truthy(value)) {
        Some(restored) => IvyJson::object([
            ("hostId", config.required("hostId")?.clone()),
            ("runtimeRoot", config.required("runtimeRoot")?.clone()),
            ("backupId", restored.required("backupId")?.clone()),
        ]),
        None => config.required("hostId")?.clone(),
    };
    Ok(format!("ivy-next-{}", &hash_json(&value)?[7..19]))
}
fn installation(value: &str) -> bool {
    value.len() == 21
        && value.starts_with("ivy-next-")
        && value.as_bytes()[9..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}
pub fn linux_resource_scope(installation_id: &str, budget: &IvyJson) -> IvyResult<IvyJson> {
    validate_host("LinuxResourceBudget", budget)?;
    require(
        installation(installation_id)
            && budget
                .required("memoryHighBytes")?
                .as_f64()
                .is_some_and(|value| {
                    value
                        < budget
                            .get("memoryMaxBytes")
                            .and_then(IvyJson::as_f64)
                            .unwrap_or(f64::NAN)
                }),
        "invalid_arguments",
        "Linux resources require an owned installation and memory high below memory max.",
    )?;
    Ok(IvyJson::object([
        (
            "slice",
            format!(
                "ivynext{}{}.slice",
                &installation_id[9..],
                &hash_json(budget)?[7..19]
            )
            .into(),
        ),
        ("budget", budget.clone()),
    ]))
}
pub fn validate_linux_resource_scope(
    scope: &IvyJson,
    installation_id: Option<&str>,
) -> IvyResult<()> {
    validate_host("LinuxResourceScope", scope)?;
    let slice = scope.required("slice")?.as_str()?;
    let identity = installation_id
        .map(str::to_owned)
        .unwrap_or_else(|| format!("ivy-next-{}", &slice[7..19]));
    require(
        hash_json(scope)?
            == hash_json(&linux_resource_scope(&identity, scope.required("budget")?)?)?,
        "target_conflict",
        "Linux resource scope differs from its installation or budget.",
    )
}
pub fn linux_slice(scope: &IvyJson) -> IvyResult<String> {
    validate_linux_resource_scope(scope, None)?;
    let budget = scope.required("budget")?;
    Ok(format!(
        "[Unit]\nDescription=IvyNext resource boundary {}\n\n[Slice]\nMemoryAccounting=true\nMemoryHigh={}\nMemoryMax={}\nMemorySwapMax=0\nTasksAccounting=true\nTasksMax={}\n",
        scope.required("slice")?.as_str()?,
        budget_text(budget.required("memoryHighBytes")?)?.to_utf8()?,
        budget_text(budget.required("memoryMaxBytes")?)?.to_utf8()?,
        budget_text(budget.required("tasksMax")?)?.to_utf8()?
    ))
}
fn slice_path(scope: &IvyJson) -> IvyResult<JsString> {
    validate_linux_resource_scope(scope, None)?;
    Ok(format!("/etc/systemd/system/{}", scope.required("slice")?.as_str()?).into())
}
async fn verify_file(host: &dyn LinuxHost, scope: &IvyJson) -> IvyResult<()> {
    let path = slice_path(scope)?;
    let metadata = internal(host.metadata(path.clone()).await)?;
    require(
        metadata.is_file
            && !metadata.is_symlink
            && metadata.uid == Some(0)
            && metadata.mode & 0o022 == 0
            && metadata.size <= 4096,
        "target_conflict",
        "Linux resource definition must be a bounded administrator-owned regular file.",
    )?;
    require(
        internal(host.read(path).await)? == linux_slice(scope)?.into_bytes(),
        "target_conflict",
        "Linux resource definition differs from its immutable budget.",
    )
}
pub async fn ensure_linux_slice(scope: IvyJson, bootstrap_root: PathBuf) -> IvyResult<()> {
    ensure_linux_slice_with_host(scope, Arc::new(NativeLinuxHost::new(bootstrap_root))).await
}
/// One admitted owner completes creation, sync and verification after a dropped waiter.
pub async fn ensure_linux_slice_with_host(
    scope: IvyJson,
    host: Arc<dyn LinuxHost>,
) -> IvyResult<()> {
    internal(
        tokio::spawn(async move {
            require(
                host.platform() == "linux" && host.user_id() == Some(0),
                "unsupported_runtime",
                "Linux resource installation needs the systemd administrator.",
            )?;
            let path = slice_path(&scope)?;
            match host.metadata(path.clone()).await {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    match host
                        .create(
                            path,
                            linux_slice(&scope)?.into_bytes(),
                            0o644,
                            "/etc/systemd/system".into(),
                        )
                        .await
                    {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(_) => return Err(IvyError::internal()),
                    }
                }
                Err(_) => return Err(IvyError::internal()),
            }
            verify_file(host.as_ref(), &scope).await
        })
        .await,
    )?
}
pub async fn inspect_linux_slice(scope: IvyJson, bootstrap_root: PathBuf) -> IvyResult<()> {
    inspect_linux_slice_with_host(scope, Arc::new(NativeLinuxHost::new(bootstrap_root))).await
}
pub async fn inspect_linux_slice_with_host(
    scope: IvyJson,
    host: Arc<dyn LinuxHost>,
) -> IvyResult<()> {
    internal(tokio::spawn(async move{
        verify_file(host.as_ref(),&scope).await?;
        let stdout=host.command("/usr/bin/systemctl",vec!["show".into(),string(scope.required("slice")?)?,"--property=FragmentPath,DropInPaths,Transient,LoadState,MemoryHigh,MemoryMax,MemorySwapMax,TasksMax".into()],10_000,16_384).await?;
        // Object.fromEntries in the original uses the final duplicate property.
        let mut values=std::collections::BTreeMap::new();
        for line in crate::linux_host::trimmed(&stdout).0.split(|unit|*unit==10){let index=line.iter().position(|unit|*unit==61);let (key,value)=match index{Some(i)=>(&line[..i],&line[i+1..]),None=>(&line[..line.len().saturating_sub(1)],line)};values.insert(JsString(key.to_vec()),JsString(value.to_vec()));}
        let budget=scope.required("budget")?;
        let expected=[("FragmentPath",slice_path(&scope)?),("DropInPaths","".into()),("Transient","no".into()),("LoadState","loaded".into()),("MemoryHigh",budget_text(budget.required("memoryHighBytes")?)?),("MemoryMax",budget_text(budget.required("memoryMaxBytes")?)?),("MemorySwapMax","0".into()),("TasksMax",budget_text(budget.required("tasksMax")?)?)];
        require(expected.into_iter().all(|(key,value)|values.get(&JsString::from(key))==Some(&value)),"target_conflict","Loaded Linux resource definition has different ownership, overrides or limits.")
    }).await)?
}
pub fn assert_linux_resource_membership(
    scope: &IvyJson,
    unit: &JsString,
    membership: &JsString,
    values: &IvyJson,
) -> IvyResult<()> {
    validate_linux_resource_scope(scope, None)?;
    let unit = unit.to_utf8().ok();
    let slice = scope.required("slice")?.as_str()?;
    let budget = scope.required("budget")?;
    let valid = unit.as_ref().is_some_and(|unit| {
        unit.len() == 42
            && unit.is_ascii()
            && unit.ends_with(".service")
            && installation(&unit[..21])
            && unit.as_bytes()[21] == b'-'
            && unit.as_bytes()[22..34]
                .iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
            && unit.starts_with(&format!("ivy-next-{}-", &slice[7..19]))
    });
    let wanted = format!("0::/{slice}/{}", unit.as_deref().unwrap_or(""));
    let controls = [
        ("memory.high", "memoryHighBytes"),
        ("memory.max", "memoryMaxBytes"),
        ("pids.max", "tasksMax"),
    ]
    .into_iter()
    .all(|(key, budget_key)| {
        values
            .get(key)
            .and_then(IvyJson::as_js_string)
            .is_some_and(|value| {
                budget_text(budget.required(budget_key).unwrap()).ok()
                    == Some(crate::linux_host::trimmed(value))
            })
    });
    require(
        valid
            && crate::linux_host::trimmed(membership) == JsString::from(wanted)
            && controls
            && values
                .get("memory.swap.max")
                .and_then(IvyJson::as_js_string)
                .is_some_and(|value| crate::linux_host::trimmed(value) == JsString::from("0")),
        "resource_scope_mismatch",
        "Runtime owner is outside its expected cgroup-v2 resource boundary or the effective kernel limits changed.",
    )
}
pub async fn verify_linux_resource_membership(
    config: IvyJson,
    instance_id: JsString,
    scope: IvyJson,
    bootstrap_root: PathBuf,
) -> IvyResult<()> {
    verify_linux_resource_membership_with_host(
        config,
        instance_id,
        scope,
        Arc::new(NativeLinuxHost::new(bootstrap_root)),
    )
    .await
}
pub async fn verify_linux_resource_membership_with_host(
    config: IvyJson,
    instance_id: JsString,
    scope: IvyJson,
    host: Arc<dyn LinuxHost>,
) -> IvyResult<()> {
    internal(
        tokio::spawn(async move {
            require(
                host.platform() == "linux",
                "unsupported_runtime",
                "Linux resource scope cannot run on another platform.",
            )?;
            let identity = installation_identity(&config)?;
            validate_linux_resource_scope(&scope, Some(&identity))?;
            let unit = format!(
                "{identity}-{}.service",
                &hash_json(&IvyJson::String(instance_id))?[7..19]
            );
            let slice = scope.required("slice")?.as_str()?;
            // Admit the complete Promise.all input before observing any error.
            let futures =
                ["memory.high", "memory.max", "memory.swap.max", "pids.max"].map(|name| {
                    let path = JsString::from(format!("/sys/fs/cgroup/{slice}/{name}"));
                    let read = host.read(path);
                    async move {
                        Ok::<_, IvyError>((
                            name,
                            IvyJson::String(JsString::from(
                                String::from_utf8_lossy(&internal(read.await)?).as_ref(),
                            )),
                        ))
                    }
                });
            let values = IvyJson::object(futures_util::future::try_join_all(futures).await?);
            let membership = internal(host.read("/proc/self/cgroup".into()).await)?;
            assert_linux_resource_membership(
                &scope,
                &unit.into(),
                &String::from_utf8_lossy(&membership).as_ref().into(),
                &values,
            )
        })
        .await,
    )?
}
