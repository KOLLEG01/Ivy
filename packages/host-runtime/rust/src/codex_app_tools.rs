//! Host-side resolution for the ordinary bundled App Tools MCP subprocess.
//! The consumer owns MCP negotiation and actor/receipt policy. No process starts here.
use ivy_contracts::{IvyError, IvyResult, JsString};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppToolsLaunchSpec {
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    /// Explicit additions to the MCP transport's standard environment.
    pub env: BTreeMap<JsString, JsString>,
    pub pipe_candidates: Vec<JsString>,
    /// Original installation identity is the selected server path, not a new hash.
    pub installation_identity: PathBuf,
}
impl AppToolsLaunchSpec {
    pub fn environment_for_pipe(&self, pipe: &JsString) -> IvyResult<BTreeMap<JsString, JsString>> {
        if !self.pipe_candidates.contains(pipe) {
            return Err(IvyError::new(
                "app_tools_pipe_unavailable",
                "No current Codex Desktop App Tools pipe is available.",
            ));
        }
        let mut env = self.env.clone();
        env.insert("CODEX_APP_TOOLS_PIPE_PATH".into(), pipe.clone());
        Ok(env)
    }
}

fn version(name: &str) -> bool {
    let parts: Vec<_> = name.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}
// Intl numeric comparison for the ASCII digit/dot-only version alphabet.
// Stable sorting retains directory order when leading-zero spellings compare equal.
fn version_order(a: &str, b: &str) -> std::cmp::Ordering {
    for (a, b) in a.split('.').zip(b.split('.')) {
        let a = a.trim_start_matches('0');
        let b = b.trim_start_matches('0');
        let order = a.len().cmp(&b.len()).then_with(|| a.cmp(b));
        if !order.is_eq() {
            return order;
        }
    }
    std::cmp::Ordering::Equal
}
pub fn latest_bundled_app_tools_server(root: &Path) -> IvyResult<PathBuf> {
    let mut versions: Vec<_> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| version(name))
        .collect();
    versions.sort_by(|a, b| version_order(b, a));
    for name in versions {
        let server = lexical(&root.join(name).join("server.mjs"));
        if server.is_file() {
            return Ok(server);
        }
    }
    Err(IvyError::new(
        "app_tools_unavailable",
        "No installed bundled Codex App Tools server is available.",
    ))
}
fn absolute(path: &Path) -> IvyResult<PathBuf> {
    let path = std::path::absolute(path).map_err(|_| IvyError::internal())?;
    Ok(lexical(&path))
}
fn lexical(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(result.components().next_back(), Some(Component::Normal(_))) {
                    result.pop();
                } else if !path.has_root() {
                    result.push("..");
                }
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}
fn units(value: &OsStr) -> JsString {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        JsString(value.encode_wide().collect())
    }
    #[cfg(not(windows))]
    {
        value.to_string_lossy().as_ref().into()
    }
}
fn folded(value: JsString) -> JsString {
    if !cfg!(windows) {
        return value;
    }
    let mut result = Vec::new();
    let mut segment = String::new();
    for ch in char::decode_utf16(value.0) {
        match ch {
            Ok(ch) => segment.push(ch),
            Err(ch) => {
                result.extend(segment.to_lowercase().encode_utf16());
                segment.clear();
                result.push(ch.unpaired_surrogate());
            }
        }
    }
    result.extend(segment.to_lowercase().encode_utf16());
    JsString(result)
}
fn bundled(server: Option<&Path>, root: &Path) -> IvyResult<bool> {
    let Some(server) = server else {
        return Ok(true);
    };
    if server.as_os_str().is_empty() {
        return Ok(true);
    }
    let mut root = folded(units(absolute(root)?.as_os_str()));
    root.0.push(std::path::MAIN_SEPARATOR as u16);
    Ok(folded(units(absolute(server)?.as_os_str()))
        .0
        .starts_with(&root.0))
}
/// Inputs come from validated settings and the host's selected Node runtime.
/// `runtime_node` is Node, never this Rust executable. Legacy hash settings are
/// deliberately absent: the original bundled resolution does not enforce them.
pub fn resolve_app_tools_launch(
    node_override: Option<&Path>,
    server_override: Option<&Path>,
    runtime_node: &Path,
    bundle_root: &Path,
    pipe_candidates: Vec<JsString>,
) -> IvyResult<AppToolsLaunchSpec> {
    let automatic = bundled(server_override, bundle_root)?;
    let server = if automatic {
        latest_bundled_app_tools_server(bundle_root)?
    } else {
        server_override.expect("explicit server").to_owned()
    };
    let node = if automatic {
        runtime_node
    } else {
        node_override.unwrap_or(runtime_node)
    };
    Ok(AppToolsLaunchSpec {
        executable: node.to_owned(),
        args: vec![server.as_os_str().to_owned()],
        cwd: server.parent().unwrap_or_else(|| Path::new(".")).to_owned(),
        env: BTreeMap::new(),
        pipe_candidates,
        installation_identity: server,
    })
}
/// Preserve explicit pipes and the current-environment-first, first-seen order.
pub fn app_tools_pipe_candidates(
    configured: &JsString,
    current: Option<&JsString>,
    names: impl IntoIterator<Item = JsString>,
) -> Vec<JsString> {
    if configured != &JsString::from("auto") {
        return vec![configured.clone()];
    }
    let root = JsString::from("\\\\.\\pipe\\");
    let prefix = JsString::from("codex-browser-use-");
    let mut full_prefix = root.0.clone();
    full_prefix.extend(&prefix.0);
    let mut pipes = Vec::new();
    if let Some(current) = current.filter(|p| p.0.starts_with(&full_prefix)) {
        pipes.push(current.clone());
    }
    for name in names {
        if !name.0.starts_with(&prefix.0) {
            continue;
        }
        let mut path = root.0.clone();
        path.extend(name.0);
        let pipe = JsString(path);
        if !pipes.contains(&pipe) {
            pipes.push(pipe);
        }
    }
    pipes
}
pub fn installed_app_tools_pipe_candidates(configured: &JsString) -> Vec<JsString> {
    let current = std::env::var_os("CODEX_APP_TOOLS_PIPE_PATH").map(|s| units(&s));
    if configured != &JsString::from("auto") {
        return vec![configured.clone()];
    }
    let names = std::fs::read_dir("\\\\.\\pipe\\")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| units(&e.file_name()));
    app_tools_pipe_candidates(configured, current.as_ref(), names)
}
