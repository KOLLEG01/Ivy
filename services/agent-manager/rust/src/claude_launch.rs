//! Existing pinned Claude adapter admission. This does not implement its provider loop.
use crate::native_launch::{native_realpath, require};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::digest, json::parse};
use ivy_host_runtime::{
    node_path::{self, Context},
    streaming_hash::file_hash,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub async fn checked_claude_adapter(
    root: &JsString,
    expected_hash: &JsString,
    paths: &Context,
) -> IvyResult<PathBuf> {
    require(
        node_path::is_absolute(paths.style, root),
        "invalid_arguments",
        "Claude adapter root must be absolute.",
    )?;
    let root = native_realpath(node_path::filesystem_path(root)).await?;
    let source = root.join("dist").join("src");
    let mut entries = tokio::fs::read_dir(&source)
        .await
        .map_err(|_| IvyError::internal())?;
    let mut names = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|_| IvyError::internal())?
    {
        let name = node_path::node_utf8(&entry.file_name());
        if name.0.ends_with(&JsString::from(".mjs").0) {
            names.push(name);
        }
    }
    require(
        names.contains(&JsString::from("adapter.mjs")),
        "native_build_mismatch",
        "Claude adapter entrypoint is missing.",
    )?;
    let mut files = vec![JsString::from("package-lock.json")];
    for name in names {
        let mut file = JsString::from("dist/src/");
        file.0.extend(name.0);
        files.push(file);
    }
    files.sort();
    let mut manifest = Vec::new();
    for name in files {
        let path = root.join(node_path::filesystem_path(&name));
        require(
            native_realpath(path.clone()).await? == path
                && tokio::fs::metadata(&path)
                    .await
                    .map_err(|_| IvyError::internal())?
                    .is_file(),
            "native_build_mismatch",
            "Claude adapter member must be a regular file.",
        )?;
        manifest.extend_from_slice(String::from_utf16_lossy(&name.0).as_bytes());
        manifest.push(0);
        let hash = file_hash(path).await?;
        // The Host file hasher returns its ordinary sha256: digest.
        manifest.extend_from_slice(hash.as_bytes().get(7..).ok_or_else(IvyError::internal)?);
        manifest.push(0);
    }
    require(
        JsString::from(digest(manifest)) == *expected_hash,
        "native_build_mismatch",
        "Claude adapter bytes differ from the pinned catalog.",
    )?;
    Ok(source.join("adapter.mjs"))
}

const PAID_CONFIGURATION_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];
pub async fn assert_claude_subscription_config(
    root: &Path,
    environment: &BTreeMap<JsString, JsString>,
) -> IvyResult<()> {
    require(
        PAID_CONFIGURATION_KEYS.iter().all(|key| {
            environment
                .iter()
                .find(|(name, _)| {
                    if cfg!(windows) {
                        name.to_english_lowercase() == JsString::from(*key).to_english_lowercase()
                    } else {
                        **name == JsString::from(*key)
                    }
                })
                .is_none_or(|(_, value)| value.0.is_empty())
        }),
        "billing_configuration_conflict",
        "Claude subscription mode conflicts with API or cloud gateway environment settings.",
    )?;
    for name in ["settings.json", "settings.local.json"] {
        let path = root.join(name);
        let info = match tokio::fs::symlink_metadata(&path).await {
            Ok(info) => info,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(IvyError::internal()),
        };
        require(
            info.is_file() && !info.is_symlink() && info.len() <= 1024 * 1024,
            "billing_configuration_conflict",
            "Claude settings must be a bounded regular file.",
        )?;
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(IvyError::internal()),
        };
        let settings = parse(&String::from_utf8_lossy(&bytes), usize::MAX).map_err(|_| {
            IvyError::new(
                "billing_configuration_conflict",
                "Claude settings are not valid JSON.",
            )
        })?;
        require(
            settings.as_object().is_some(),
            "billing_configuration_conflict",
            "Claude settings must contain an object.",
        )?;
        let env = settings.get("env").filter(|env| env.as_object().is_some());
        require(
            !PAID_CONFIGURATION_KEYS
                .iter()
                .any(|key| env.is_some_and(|env| env.get(key).is_some()))
                && settings.get("apiKeyHelper").is_none()
                && settings.get("forceLoginMethod") != Some(&IvyJson::from("console")),
            "billing_configuration_conflict",
            "Claude subscription mode conflicts with API or cloud gateway settings.",
        )?;
    }
    Ok(())
}
