//! Prepare verified direct MCP starts before native thread loading. No relay survives.
use crate::{
    environment_text::{
        BEGIN_BUNDLED as BEGIN, END_BUNDLED as END, managed_block, server_sections,
    },
    mcp_files::{conflict, internal, regular_bytes, replace_config, state_root, text},
};
use ivy_contracts::{
    IvyJson as J, IvyResult,
    hash::digest,
    json::{parse, stringify},
};
use ivy_host_runtime::{
    executor_lock::ExecutorLock,
    node_path,
    node_runtime::NodeRuntime,
    portable_native_files::{NativeHelperLayout, component_native_helper},
    process::{self, Command, LaunchContext},
};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, OnceCell};

const RESOLVER: &str = include_str!("../assets/bundled-tools.mjs");
#[derive(Clone)]
pub struct BundledTools(Arc<Inner>);
struct Inner {
    home: PathBuf,
    artifact: PathBuf,
    settings: J,
    node: OnceCell<NodeRuntime>,
    serial: Mutex<()>,
}
impl BundledTools {
    pub fn new(home: PathBuf, artifact: PathBuf, settings: J) -> IvyResult<Self> {
        if !cfg!(windows) {
            return Err(conflict(
                "Bundled tools require the registered Windows app runtime.",
            ));
        }
        Ok(Self(Arc::new(Inner {
            home,
            artifact,
            settings,
            node: OnceCell::new(),
            serial: Mutex::new(()),
        })))
    }
    pub async fn refresh(&self) -> IvyResult<()> {
        let _serial = self.0.serial.lock().await;
        let node_path = node_path::filesystem_path(
            self.0
                .settings
                .required("nodeExecutable")?
                .as_js_string()
                .ok_or_else(ivy_contracts::IvyError::internal)?,
        );
        let node = self
            .0
            .node
            .get_or_try_init(|| NodeRuntime::discover(Some(&node_path)))
            .await?;
        let helper =
            component_native_helper(&self.0.artifact, "ivy-job.exe", NativeHelperLayout::Target)?;
        let result = process::run_command(
            &Command {
                executable: "$node".into(),
                args: vec![
                    "--input-type=module".into(),
                    "-e".into(),
                    RESOLVER.into(),
                    "--".into(),
                    "--ivy-bundled-tools".into(),
                    self.0.home.clone().into_os_string(),
                ],
                cwd: None,
                timeout_ms: 30_000,
            },
            &self.0.artifact,
            &BTreeMap::from([("$node".into(), node.executable.clone())]),
            LaunchContext {
                job_launcher: Some(helper),
                maximum_output_bytes: Some(128 * 1024),
                ..Default::default()
            },
            None,
        )
        .await?;
        if result.exit_code != Some(0) || result.error_code.is_some() || result.truncated {
            return Err(conflict(
                "Bundled tool runtime verification failed; provision the registered app's matching cache.",
            ));
        }
        let resolved = parse(&result.stdout, 128 * 1024)?;
        let block = render(&resolved, &self.0.settings)?;
        let home = self.0.home.clone();
        let root = home.join(".ivy-agent-mcp");
        let check = root.clone();
        internal(tokio::task::spawn_blocking(move || state_root(&check)).await)??;
        let lock = ExecutorLock::acquire(root, false).await?;
        internal(tokio::task::spawn_blocking(move || {
            let _lock = lock;
            let path = home.join("config.toml");
            let before = regular_bytes(&path)?;
            let source = text(before.as_deref())?;
            let current = managed_block(source, BEGIN, END)?;
            if server_sections(&current.outside).iter().any(|s| ["node_repl", "cua_repl"].contains(&s.as_str())) {
                return Err(conflict("Bundled tool servers require explicit migration of their existing MCP definitions."));
            }
            let state_path = home.join(".ivy-agent-mcp/bundled-tools.json");
            let state = regular_bytes(&state_path)?.map(|b| parse(text(Some(&b))?, usize::MAX)).transpose()?;
            let applied = state.as_ref().and_then(|v| v.get("blockHash"));
            let desired = J::from(digest(&block));
            if let Some(old) = current.block.as_ref() {
                let old = J::from(digest(old));
                if Some(&old) != applied && old != desired { return Err(conflict("Bundled tool configuration has local changes.")); }
            } else if applied.is_some() { return Err(conflict("Bundled tool configuration was removed.")); }
            // This block contains tables only. Append it outside the ordinary
            // MCP owner's markers; inserting before its first table would nest
            // the two owners and invalidate that owner's retained block hash.
            let after = append_block(&current.outside, &block);
            if before.as_deref() != Some(after.as_bytes()) { replace_config(&path, before.as_deref(), after.as_bytes())?; }
            internal(ivy_store::atomic::atomic_json(&state_path, &J::object([("blockHash", desired), ("runtime", resolved)])))
        }).await)?
    }
}
pub fn append_block(outside: &str, block: &str) -> String {
    let outside = outside.trim_end();
    if outside.is_empty() {
        format!("{block}\n")
    } else {
        format!("{outside}\n\n{block}\n")
    }
}
pub fn render(runtime: &J, settings: &J) -> IvyResult<String> {
    let mut sections = vec![BEGIN.to_owned()];
    for (name, env_key, args_key) in [
        ("node_repl", "nodeEnvironment", "nodeArgs"),
        ("cua_repl", "cuaEnvironment", "cuaArgs"),
    ] {
        let spec = runtime.required("servers")?.required(name)?;
        let mut args = spec
            .required("args")?
            .as_array()
            .ok_or_else(ivy_contracts::IvyError::internal)?
            .to_vec();
        args.extend_from_slice(
            settings
                .required(args_key)?
                .as_array()
                .ok_or_else(ivy_contracts::IvyError::internal)?,
        );
        sections.push(format!("[mcp_servers.{name}]\ncommand = {}\nargs = {}\nenv_vars = {}\nstartup_timeout_sec = {}\ntool_timeout_sec = {}", stringify(spec.required("command")?, usize::MAX)?, stringify(&J::Array(args), usize::MAX)?, stringify(settings.required("inheritedEnvironment")?, usize::MAX)?, stringify(settings.required("startupTimeoutSeconds")?, usize::MAX)?, stringify(settings.required("toolTimeoutSeconds")?, usize::MAX)?));
        sections.push(format!("[mcp_servers.{name}.env]"));
        let mut env = settings.required(env_key)?.clone();
        for (key, value) in spec
            .required("env")?
            .as_object()
            .ok_or_else(ivy_contracts::IvyError::internal)?
        {
            env.insert(key.clone(), value.clone());
        }
        for (key, value) in env
            .as_object()
            .ok_or_else(ivy_contracts::IvyError::internal)?
        {
            sections.push(format!(
                "{} = {}",
                stringify(&J::String(key.clone()), usize::MAX)?,
                stringify(value, usize::MAX)?
            ));
        }
    }
    sections.push(END.to_owned());
    Ok(sections.join("\n"))
}
