//! Codex home selection and explicit instance ownership, without filesystem effects.
use super::node_path::{self, Context, Style};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct HomeSettings {
    pub codex_home: Option<JsString>,
    pub native_home: Option<JsString>,
}
pub enum AppServerMode {
    Default,
    OwnedStdio,
    ExternalProxy {
        expected_codex_home: Option<JsString>,
    },
    ClaudeAdapter,
}
pub struct HomeContext<'a> {
    pub paths: &'a Context,
    pub environment: &'a BTreeMap<JsString, JsString>,
    pub user_home: Option<&'a JsString>,
    /// Explicit options.cwd or raw native process cwd: a resolve argument,
    /// distinct from the path engine's style-specific fallback cwd.
    pub cwd: &'a JsString,
    /// The host's executing-account home, equivalent to os.homedir(). Never a
    /// deployment-global home or a service data directory substituted as fallback.
    pub account_home: &'a JsString,
}
#[derive(Debug)]
pub struct HomeResolution {
    pub path: JsString,
    pub source: &'static str,
}
fn invalid(message: &str) -> IvyError {
    IvyError::new("invalid_arguments", message)
}
fn space(c: u16) -> bool {
    char::from_u32(u32::from(c)).is_some_and(char::is_whitespace)
}
fn trim(s: &JsString) -> JsString {
    let start = s.0.iter().position(|c| !space(*c)).unwrap_or(s.0.len());
    let end =
        s.0.iter()
            .rposition(|c| !space(*c))
            .map_or(start, |i| i + 1);
    JsString(s.0[start..end].to_vec())
}
fn home_identity(style: Style, path: &JsString) -> JsString {
    let normalized = node_path::normalize(style, path);
    if style != Style::Windows {
        return normalized;
    }
    // Apply Unicode lowercasing to scalar runs while retaining isolated UTF-16
    // surrogates. Normalized trailing separators remain part of the identity.
    let mut result = Vec::new();
    let mut segment = String::new();
    for c in char::decode_utf16(normalized.0) {
        match c {
            Ok(c) => segment.push(c),
            Err(e) => {
                result.extend(segment.to_lowercase().encode_utf16());
                segment.clear();
                result.push(e.unpaired_surrogate());
            }
        }
    }
    result.extend(segment.to_lowercase().encode_utf16());
    JsString(result)
}
pub fn uses_external_app_server(mode: &AppServerMode) -> bool {
    !matches!(
        mode,
        AppServerMode::OwnedStdio | AppServerMode::ClaudeAdapter
    )
}

/// Settings are schema-validated strings. Context is an explicit snapshot of
/// the executing account's path/environment inputs; this call changes none.
pub fn resolve_codex_home(
    settings: &HomeSettings,
    context: &HomeContext<'_>,
) -> IvyResult<HomeResolution> {
    let style = context.paths.style;
    if let (Some(a), Some(b)) = (&settings.codex_home, &settings.native_home) {
        if !a.0.is_empty() && !b.0.is_empty() && home_identity(style, a) != home_identity(style, b)
        {
            return Err(invalid(
                "codexHome and the existing explicit nativeHome must not select different homes.",
            ));
        }
    }
    if let Some(configured) = settings
        .codex_home
        .as_ref()
        .or(settings.native_home.as_ref())
    {
        if trim(configured).0.is_empty() || !node_path::is_absolute(style, configured) {
            return Err(invalid("Explicit Codex home must be an absolute path."));
        }
        return Ok(HomeResolution {
            path: node_path::normalize(style, configured),
            source: if settings.codex_home.is_some() {
                "host-configuration"
            } else {
                "existing-native-home"
            },
        });
    }
    if let Some(inherited) = context
        .environment
        .get(&JsString::from("CODEX_HOME"))
        .map(trim)
        .filter(|s| !s.0.is_empty())
    {
        return Ok(HomeResolution {
            path: context.paths.resolve(&[context.cwd.clone(), inherited]),
            source: "environment",
        });
    }
    let user_home = context
        .user_home
        .or_else(|| {
            context
                .environment
                .get(&JsString::from(if style == Style::Windows {
                    "USERPROFILE"
                } else {
                    "HOME"
                }))
        })
        .unwrap_or(context.account_home);
    if !node_path::is_absolute(style, user_home) {
        return Err(invalid(
            "The executing account must have an absolute home directory.",
        ));
    }
    Ok(HomeResolution {
        path: node_path::join(style, &[user_home.clone(), ".codex".into()]),
        source: "user-home",
    })
}
pub fn expected_server_home(
    settings: &HomeSettings,
    mode: &AppServerMode,
    context: &HomeContext<'_>,
) -> IvyResult<JsString> {
    if let AppServerMode::ExternalProxy {
        expected_codex_home: Some(home),
    } = mode
    {
        return Ok(home.clone());
    }
    Ok(resolve_codex_home(settings, context)?.path)
}

/// Environment/default homes never grant ownership, nor does selecting the
/// instance data root itself. Only an explicit strict descendant is owned.
pub fn instance_owned_codex_home(
    settings: &HomeSettings,
    data_root: &JsString,
    context: &HomeContext<'_>,
) -> IvyResult<Option<JsString>> {
    if settings.codex_home.is_none() && settings.native_home.is_none() {
        return Ok(None);
    }
    let home = resolve_codex_home(settings, context)?.path;
    Ok(owned_home_at(home, data_root, context))
}

fn settings_object(settings: &IvyJson) -> IvyResult<&IvyJson> {
    if settings.as_object().is_none() {
        return Err(invalid("Codex settings must be an object."));
    }
    Ok(settings)
}
fn home_settings(settings: &IvyJson) -> IvyResult<HomeSettings> {
    let settings = settings_object(settings)?;
    let optional_home = |name: &str| match settings.get(name) {
        None => Ok(None),
        Some(IvyJson::String(home)) => Ok(Some(home.clone())),
        Some(_) => Err(invalid(&format!("{name} must be a string when supplied."))),
    };
    Ok(HomeSettings {
        codex_home: optional_home("codexHome")?,
        native_home: optional_home("nativeHome")?,
    })
}
pub(crate) fn app_server(settings: &IvyJson) -> IvyResult<Option<&IvyJson>> {
    let settings = settings_object(settings)?;
    let Some(server) = settings.get("appServer") else {
        return Ok(None);
    };
    if server.as_object().is_none() {
        return Err(invalid("appServer must be an object when supplied."));
    }
    if server
        .get("mode")
        .is_some_and(|mode| mode.as_js_string().is_none())
    {
        return Err(invalid("appServer.mode must be a string when supplied."));
    }
    Ok(Some(server))
}

/// Admit the declared home fields from extensible HostConfig settings, then use
/// the same native selection and ownership rules as typed callers.
pub fn resolve_codex_home_json(
    settings: &IvyJson,
    context: &HomeContext<'_>,
) -> IvyResult<HomeResolution> {
    resolve_codex_home(&home_settings(settings)?, context)
}

pub fn uses_external_app_server_json(settings: &IvyJson) -> IvyResult<bool> {
    let mode = app_server(settings)?.and_then(|server| server.get("mode"));
    Ok(mode != Some(&IvyJson::from("owned-stdio"))
        && mode != Some(&IvyJson::from("claude-adapter")))
}

/// External expected homes are returned verbatim, including null/non-string
/// JSON. Host's shared-home validation owns their absolute-path admission.
pub fn expected_server_home_json(
    settings: &IvyJson,
    context: &HomeContext<'_>,
) -> IvyResult<IvyJson> {
    if let Some(server) = app_server(settings)? {
        if server.get("mode") == Some(&IvyJson::from("external-proxy")) {
            if let Some(home) = server.get("expectedCodexHome") {
                return Ok(home.clone());
            }
        }
    }
    resolve_codex_home_json(settings, context).map(|home| IvyJson::String(home.path))
}

pub fn instance_owned_codex_home_json(
    settings: &IvyJson,
    data_root: &JsString,
    context: &HomeContext<'_>,
) -> IvyResult<Option<JsString>> {
    instance_owned_codex_home(&home_settings(settings)?, data_root, context)
}

/// Service data is needed only after an explicit home has been resolved.
pub fn instance_owned_codex_home_data_json(
    settings: &IvyJson,
    data_root: Option<&IvyJson>,
    context: &HomeContext<'_>,
) -> IvyResult<Option<JsString>> {
    let settings = home_settings(settings)?;
    if settings.codex_home.is_none() && settings.native_home.is_none() {
        return Ok(None);
    }
    let home = resolve_codex_home(&settings, context)?.path;
    let data_root = data_root
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)?;
    Ok(owned_home_at(home, data_root, context))
}

fn owned_home_at(
    home: JsString,
    data_root: &JsString,
    context: &HomeContext<'_>,
) -> Option<JsString> {
    let local = context.paths.relative(data_root, &home);
    let outside = if context.paths.style == Style::Windows {
        [46, 46, 92]
    } else {
        [46, 46, 47]
    };
    (!local.0.is_empty()
        && local.0 != [46, 46]
        && !local.0.starts_with(&outside)
        && !node_path::is_absolute(context.paths.style, &local))
    .then_some(home)
}
