//! Grants for dedicated non-administrative Linux native service identities.
use crate::{
    IvyError, IvyJson, IvyResult,
    linux_host::{LinuxHost, NativeLinuxHost},
    node_path::{self, Style},
};
use ivy_contracts::JsString;
use std::{collections::HashSet, io, path::PathBuf, sync::Arc};
pub const LINUX_AGENT_USER: &str = "ivy-agent";
pub const LINUX_DASHBOARDS_USER: &str = "ivy-dashboards";
pub fn linux_process_user(component_id: &JsString, platform: &str) -> Option<&'static str> {
    if platform != "linux" {
        return None;
    }
    match component_id.to_utf8().ok()?.as_str() {
        "agent-manager" => Some(LINUX_AGENT_USER),
        "dashboards" => Some(LINUX_DASHBOARDS_USER),
        _ => None,
    }
}
fn text(value: &IvyJson) -> IvyResult<JsString> {
    value.as_js_string().cloned().ok_or_else(IvyError::internal)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
async fn present(host: &dyn LinuxHost, path: &JsString) -> IvyResult<bool> {
    match host.metadata(path.clone()).await {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(IvyError::internal()),
    }
}
fn concat(parts: &[JsString]) -> JsString {
    JsString(
        parts
            .iter()
            .flat_map(|part| part.0.iter().copied())
            .collect(),
    )
}
async fn parents(host: &dyn LinuxHost, user: &JsString, path: &JsString) -> IvyResult<()> {
    let mut current = node_path::dirname(Style::Posix, path);
    loop {
        let parent = node_path::dirname(Style::Posix, &current);
        if current == parent {
            break;
        }
        host.command(
            "/usr/bin/setfacl",
            vec![
                "-m".into(),
                concat(&["u:".into(), user.clone(), ":--x".into()]),
                current,
            ],
            60_000,
            65_536,
        )
        .await?;
        current = parent;
    }
    Ok(())
}
async fn file(
    host: &dyn LinuxHost,
    user: &JsString,
    path: JsString,
    permissions: &str,
) -> IvyResult<()> {
    require(
        present(host, &path).await?,
        "not_found",
        "A Linux service identity input file is missing.",
    )?;
    parents(host, user, &path).await?;
    host.command(
        "/usr/bin/setfacl",
        vec![
            "-m".into(),
            concat(&["u:".into(), user.clone(), ":".into(), permissions.into()]),
            path,
        ],
        60_000,
        65_536,
    )
    .await?;
    Ok(())
}
async fn tree(
    host: &dyn LinuxHost,
    user: &JsString,
    path: JsString,
    writable: bool,
) -> IvyResult<()> {
    if !present(host, &path).await? {
        return Ok(());
    }
    parents(host, user, &path).await?;
    host.command(
        "/usr/bin/setfacl",
        vec![
            "-R".into(),
            "-m".into(),
            concat(&[
                "u:".into(),
                user.clone(),
                if writable { ":rwX" } else { ":r-X" }.into(),
            ]),
            path.clone(),
        ],
        60_000,
        65_536,
    )
    .await?;
    if writable {
        host.command(
            "/usr/bin/setfacl",
            vec![
                "-R".into(),
                "-m".into(),
                concat(&["d:u:".into(), user.clone(), ":rwx".into()]),
                path,
            ],
            60_000,
            65_536,
        )
        .await?;
    }
    Ok(())
}
async fn own_tree(
    host: &dyn LinuxHost,
    user: &JsString,
    path: JsString,
    uid: f64,
    gid: f64,
) -> IvyResult<()> {
    if !present(host, &path).await? {
        return Ok(());
    }
    let metadata = host
        .metadata(path.clone())
        .await
        .map_err(|_| IvyError::internal())?;
    if metadata.uid.map(f64::from) == Some(uid) && metadata.gid.map(f64::from) == Some(gid) {
        return Ok(());
    }
    host.command(
        "/usr/bin/chown",
        vec![
            "-R".into(),
            concat(&[user.clone(), ":".into(), user.clone()]),
            path,
        ],
        60_000,
        65_536,
    )
    .await?;
    Ok(())
}
fn roots(runtime: &IvyJson, settings: &IvyJson, projects: bool) -> Vec<JsString> {
    let mut roots: Vec<_> = ["dataRoot", "workRoot", "logsRoot"]
        .into_iter()
        .filter_map(|key| runtime.get(key).and_then(IvyJson::as_js_string).cloned())
        .collect();
    if projects {
        roots.extend(
            ["projectRoot", "internalProjectRoot"]
                .into_iter()
                .filter_map(|key| settings.get(key).and_then(IvyJson::as_js_string).cloned()),
        );
        if let Some(projects) = settings.get("projects").and_then(IvyJson::as_array) {
            roots.extend(
                projects
                    .iter()
                    .filter(|project| project.as_object().is_some())
                    .filter_map(|project| {
                        project.get("path").and_then(IvyJson::as_js_string).cloned()
                    }),
            );
        }
    }
    let mut seen = HashSet::new();
    roots.retain(|root| seen.insert(root.clone()));
    roots
}
pub async fn ensure_linux_process_identity(
    identity: IvyJson,
    runtime: IvyJson,
    bootstrap_root: PathBuf,
) -> IvyResult<()> {
    ensure_linux_process_identity_with_host(
        identity,
        runtime,
        Arc::new(NativeLinuxHost::new(bootstrap_root)),
    )
    .await
}
pub async fn ensure_linux_process_identity_with_host(
    identity: IvyJson,
    runtime: IvyJson,
    host: Arc<dyn LinuxHost>,
) -> IvyResult<()> {
    tokio::spawn(async move{
        let Some(user)=identity.get("user").filter(|value|!value.is_null())else{return Ok(())};
        let expected_user=identity.get("componentId").and_then(IvyJson::as_js_string).and_then(|component|linux_process_user(component,"linux"));
        require(host.platform()=="linux"&&host.user_id()==Some(0)&&expected_user.is_some_and(|expected|user==&IvyJson::from(expected)),"unsupported_runtime","Linux service identity setup requires the system administrator and the configured service identity.")?;
        let user=text(user)?;
        let config_path=text(identity.required("configPath")?)?;
        let artifact_root=text(identity.required("artifactRoot")?)?;
        let executable=identity.get("processExecutable").or_else(||identity.get("nodeExecutable")).filter(|value|!value.is_null()).map(text).transpose()?;
        let settings=runtime.required("settings")?;
        let native=settings.get("nativeExecutable").or_else(||settings.get("chromiumExecutable")).filter(|value|!value.is_null()).map(text).transpose()?;
        let uid=match host.command("/usr/bin/id",vec!["-u".into(),user.clone()],10_000,65_536).await{
            Ok(value)=>native_id(&value)?,
            Err(_)=>{
                host.command("/usr/sbin/useradd",vec!["--system".into(),"--user-group".into(),"--create-home".into(),"--home-dir".into(),concat(&["/var/lib/".into(),user.clone()]),"--shell".into(),"/usr/sbin/nologin".into(),user.clone()],30_000,65_536).await?;
                native_id(&host.command("/usr/bin/id",vec!["-u".into(),user.clone()],10_000,65_536).await?)?
            }
        };
        let gid=native_id(&host.command("/usr/bin/id",vec!["-g".into(),user.clone()],10_000,65_536).await?)?;
        require([uid,gid].into_iter().all(|value|value.is_finite()&&value.fract()==0.0&&value>0.0&&value<=9_007_199_254_740_991.0),"access_denied","The service must use a non-administrative Linux user.")?;
        file(host.as_ref(),&user,config_path,"r--").await?;
        if let Some(executable)=executable{file(host.as_ref(),&user,executable,"r-x").await?;}
        if let Some(native)=native{file(host.as_ref(),&user,native,"r-x").await?;}
        tree(host.as_ref(),&user,artifact_root,false).await?;
        for root in roots(&runtime,settings,false){own_tree(host.as_ref(),&user,root,uid,gid).await?;}
        for root in roots(&runtime,settings,true){tree(host.as_ref(),&user,root,true).await?;}
        Ok(())
    }).await.map_err(|_|IvyError::internal())?
}
fn native_id(value: &JsString) -> IvyResult<f64> {
    value
        .to_utf8()?
        .trim()
        .parse::<u32>()
        .map(f64::from)
        .map_err(|_| {
            IvyError::new(
                "access_denied",
                "Linux identity command returned an invalid numeric ID.",
            )
        })
}
