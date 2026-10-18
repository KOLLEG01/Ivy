use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    core_schema::validate_ui_definition,
    hash::{digest, hash_json},
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
    operation::{derive_operation_id_js, parse_operation_id_js},
};
use ivy_sdk::rpc::{ClientOptions, HiveClient, RequestOptions, RpcClient};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct FileAsset {
    pub path: String,
    pub media_type: String,
    pub content_hash: String,
    pub byte_length: usize,
}
impl FileAsset {
    fn scanned(&self) -> IvyJson {
        IvyJson::object([
            ("path", self.path.clone().into()),
            ("mediaType", self.media_type.clone().into()),
            ("contentHash", self.content_hash.clone().into()),
            ("byteLength", IvyJson::Number(self.byte_length as f64)),
        ])
    }
    fn staged(&self) -> IvyJson {
        IvyJson::object([
            ("path", self.path.clone().into()),
            ("contentHash", self.content_hash.clone().into()),
            ("mediaType", self.media_type.clone().into()),
            ("byteLength", IvyJson::Number(self.byte_length as f64)),
        ])
    }
}
#[derive(Clone, Debug)]
pub struct UiBundle {
    pub assets: Vec<FileAsset>,
    pub bundle_hash: String,
    pub release_id: String,
}
impl UiBundle {
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            (
                "assets",
                IvyJson::Array(self.assets.iter().map(FileAsset::scanned).collect()),
            ),
            ("bundleHash", self.bundle_hash.clone().into()),
            ("releaseId", self.release_id.clone().into()),
        ])
    }
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn io<T>(result: std::io::Result<T>) -> IvyResult<T> {
    result.map_err(|_| IvyError::internal())
}
fn absolute(path: &Path) -> IvyResult<PathBuf> {
    let source = io(std::path::absolute(path))?;
    let mut result = PathBuf::new();
    for component in source.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            _ => result.push(component.as_os_str()),
        }
    }
    Ok(result)
}
fn real_path(path: &Path) -> IvyResult<PathBuf> {
    let result = io(fs::canonicalize(path))?;
    #[cfg(windows)]
    {
        let text = result.to_str().ok_or_else(IvyError::internal)?;
        if let Some(tail) = text.strip_prefix("\\\\?\\UNC\\") {
            return Ok(PathBuf::from(format!("\\\\{tail}")));
        }
        if let Some(tail) = text.strip_prefix("\\\\?\\") {
            return Ok(PathBuf::from(tail));
        }
    }
    Ok(result)
}
fn media_type(path: &str) -> Option<&'static str> {
    if path == "LICENSE" {
        return Some("text/plain");
    }
    Some(match Path::new(path).extension()?.to_str()? {
        "html" => "text/html",
        "css" => "text/css",
        "js" => "text/javascript",
        "json" => "application/json",
        "txt" | "md" => "text/plain",
        "wasm" => "application/wasm",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => return None,
    })
}
fn canonical_json(bytes: &[u8], maximum: usize, decode_bom: bool) -> IvyResult<Vec<u8>> {
    let text = std::str::from_utf8(bytes).map_err(|_| IvyError::internal())?;
    let text = if decode_bom {
        text.strip_prefix('\u{feff}').unwrap_or(text)
    } else {
        text
    };
    let value = parse(text, usize::MAX).map_err(|_| IvyError::internal())?;
    Ok(canonical(&value, maximum)?.into_bytes())
}

/// Blocking filesystem scan. Async callers must dispatch this to a blocking worker.
pub fn read_ui_bundle(directory: &Path, definition: &IvyJson) -> IvyResult<UiBundle> {
    validate_ui_definition(definition)?;
    let root = absolute(directory)?;
    require(
        real_path(&root)? == root && !io(fs::symlink_metadata(&root))?.is_symlink(),
        "invalid_arguments",
        "UI bundle must be an actual directory, not a link.",
    )?;
    let prefix = format!(
        "{}{}",
        root.to_str().ok_or_else(IvyError::internal)?,
        std::path::MAIN_SEPARATOR
    );
    // Each directory is scanned once; pending entries retain the original Dirent type.
    fn entries(root: &Path, relative: &str) -> IvyResult<Vec<(String, fs::FileType)>> {
        let mut result = Vec::new();
        for entry in io(fs::read_dir(root.join(relative)))? {
            let entry = io(entry)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| IvyError::internal())?;
            result.push((format!("{relative}{name}"), io(entry.file_type())?));
        }
        // libuv scandir sorts UTF-8 names before Node's Dirent conversion.
        result.sort_by(|a, b| a.0.cmp(&b.0));
        result.reverse();
        Ok(result)
    }
    let mut pending = entries(&root, "")?;
    let mut assets = Vec::new();
    let mut total = 0usize;
    while let Some((path, kind)) = pending.pop() {
        let full = absolute(&root.join(&path))?;
        require(
            full.to_str().is_some_and(|s| s.starts_with(&prefix))
                && !kind.is_symlink()
                && !path.chars().any(|c| {
                    matches!(c, '\\' | '%' | '?' | '#' | ':') || c <= '\u{1f}' || c == '\u{7f}'
                })
                && icu_normalizer::ComposingNormalizer::new_nfc().is_normalized(&path),
            "invalid_arguments",
            "Bundle paths must be normalized relative files inside the selected directory.",
        )?;
        if kind.is_dir() {
            pending.extend(entries(&root, &format!("{path}/"))?);
            continue;
        }
        require(
            kind.is_file(),
            "invalid_arguments",
            "Bundle contains a non-file entry.",
        )?;
        if path == "ivy-ui.json" {
            continue;
        }
        let media = media_type(&path).ok_or_else(|| {
            IvyError::new(
                "invalid_arguments",
                format!("Unsupported static asset extension: {path}"),
            )
        })?;
        require(
            io(fs::symlink_metadata(&full))?.len() <= 8 * 1024 * 1024,
            "content_too_large",
            "Static asset exceeds the binary limit.",
        )?;
        let bytes = io(fs::read(&full))?;
        require(
            real_path(&full)? == full,
            "invalid_arguments",
            "Asset path changed outside the bundle.",
        )?;
        let bytes = if media == "application/json" {
            canonical_json(&bytes, 1024 * 1024, true)?
        } else {
            if media.starts_with("text/") {
                std::str::from_utf8(&bytes).map_err(|_| IvyError::internal())?;
            }
            bytes
        };
        let limit = if media.starts_with("text/") || media == "application/json" {
            1024 * 1024
        } else {
            8 * 1024 * 1024
        };
        require(
            bytes.len() <= limit,
            "content_too_large",
            "Static asset exceeds its content limit.",
        )?;
        total += bytes.len();
        require(
            total <= 32 * 1024 * 1024 && assets.len() < 2000,
            "limit_exceeded",
            "Static bundle exceeds its operating bound.",
        )?;
        assets.push(FileAsset {
            path,
            media_type: media.into(),
            content_hash: digest(&bytes),
            byte_length: bytes.len(),
        });
    }
    assets.sort_by(|a, b| a.path.encode_utf16().cmp(b.path.encode_utf16()));
    require(
        assets.iter().any(|a| {
            Some(&IvyJson::from(a.path.as_str())) == definition.get("entryPath")
                && a.media_type == "text/html"
        }),
        "invalid_arguments",
        "The ui needs an HTML entry in its complete bundle.",
    )?;
    let bundle_hash = hash_json(&IvyJson::object([
        ("definition", definition.clone()),
        (
            "assets",
            IvyJson::Array(assets.iter().map(FileAsset::scanned).collect()),
        ),
    ]))?;
    Ok(UiBundle {
        release_id: bundle_hash[7..].into(),
        bundle_hash,
        assets,
    })
}

pub struct PublishUiInput<'a> {
    pub directory: &'a Path,
    pub definition: &'a IvyJson,
    pub expected_release_id: Option<&'a JsString>,
    pub mutation_id: &'a JsString,
    pub checked_bundle: Option<&'a UiBundle>,
}
/// Register, stage and deploy sequentially with the original derived mutation identities.
pub async fn publish_ui(client: &dyn RpcClient, input: PublishUiInput<'_>) -> IvyResult<IvyJson> {
    parse_operation_id_js(input.mutation_id)?;
    let bundle = if let Some(bundle) = input.checked_bundle {
        bundle.clone()
    } else {
        let directory = input.directory.to_owned();
        let definition = input.definition.clone();
        tokio::task::spawn_blocking(move || read_ui_bundle(&directory, &definition))
            .await
            .map_err(|_| IvyError::internal())??
    };
    let definition = input.definition;
    let contracts = definition
        .get("dataContracts")
        .and_then(IvyJson::as_array)
        .ok_or_else(IvyError::internal)?;
    for contract in contracts {
        require(
            contract.get("owner").and_then(|o| o.get("kind")) == Some(&IvyJson::from("agent")),
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
                        IvyJson::String(derive_operation_id_js(input.mutation_id, &scope)?),
                    ),
                    ("definition", contract.clone()),
                ]),
                RequestOptions::default(),
            )
            .await?;
    }
    let mut assets = Vec::new();
    for item in &bundle.assets {
        let asset = item.staged();
        let bytes = io(tokio::fs::read(input.directory.join(&item.path)).await)?;
        // Buffer.toString is lossy at publication, unlike the strict initial scan.
        let bytes = if item.media_type == "application/json" {
            canonical_json(
                String::from_utf8_lossy(&bytes).as_bytes(),
                MANAGEMENT_FRAME_BYTES,
                false,
            )?
        } else {
            bytes
        };
        require(
            bytes.len() == item.byte_length && digest(&bytes) == item.content_hash,
            "artifact_changed",
            "UI bundle changed after validation.",
        )?;
        let scope = IvyJson::Array(vec![
            "asset".into(),
            item.path.clone().into(),
            item.content_hash.clone().into(),
        ]);
        client
            .request(
                "uis.stageAsset",
                IvyJson::object([
                    (
                        "uiId",
                        definition.required("metadata")?.required("uiId")?.clone(),
                    ),
                    ("releaseId", bundle.release_id.clone().into()),
                    ("asset", asset.clone()),
                    ("base64", STANDARD.encode(&bytes).into()),
                    (
                        "mutationId",
                        IvyJson::String(derive_operation_id_js(input.mutation_id, &scope)?),
                    ),
                ]),
                RequestOptions::default(),
            )
            .await?;
        assets.push(asset);
    }
    client
        .request(
            "uis.deploy",
            IvyJson::object([
                ("mutationId", IvyJson::String(input.mutation_id.clone())),
                (
                    "expectedReleaseId",
                    input
                        .expected_release_id
                        .map(|id| IvyJson::String(id.clone()))
                        .unwrap_or(IvyJson::Null),
                ),
                ("metadata", definition.required("metadata")?.clone()),
                (
                    "release",
                    IvyJson::object([
                        ("releaseId", bundle.release_id.into()),
                        ("entryPath", definition.required("entryPath")?.clone()),
                        ("requirements", definition.required("requirements")?.clone()),
                        ("assets", IvyJson::Array(assets)),
                    ]),
                ),
            ]),
            RequestOptions::default(),
        )
        .await
}

pub async fn command(
    args: &[String],
    environment: impl Fn(&str) -> Option<String>,
) -> IvyResult<IvyJson> {
    let mut values = std::collections::HashMap::new();
    let mut initial = false;
    let mut index = 0;
    while index < args.len() {
        let option = args[index]
            .strip_prefix("--")
            .ok_or_else(IvyError::internal)?;
        if option.is_empty() && index + 1 == args.len() {
            break;
        }
        let (name, inline) = option
            .split_once('=')
            .map_or((option, None), |(n, v)| (n, Some(v)));
        if name == "initial" {
            if inline.is_some() {
                return Err(IvyError::internal());
            }
            initial = true;
            index += 1;
            continue;
        }
        if !matches!(
            name,
            "directory" | "base" | "credential-env" | "mutation-id" | "expected-release"
        ) {
            return Err(IvyError::internal());
        }
        let value = if let Some(value) = inline {
            value
        } else {
            index += 1;
            let value = args.get(index).ok_or_else(IvyError::internal)?;
            if value.starts_with('-') && value != "-" {
                return Err(IvyError::internal());
            }
            value
        };
        values.insert(name, value.to_owned());
        index += 1;
    }
    require(
        ["directory", "base", "mutation-id"]
            .iter()
            .all(|key| values.get(key).is_some_and(|v| !v.is_empty()))
            && initial != values.contains_key("expected-release"),
        "invalid_arguments",
        "Use --directory, --base, --mutation-id and exactly one of --initial / --expected-release.",
    )?;
    let credential = environment(
        values
            .get("credential-env")
            .map(String::as_str)
            .unwrap_or("IVY_PUBLISH_CREDENTIAL"),
    );
    require(
        credential.as_ref().is_some_and(|s| !s.is_empty()),
        "unauthenticated",
        "The selected credential environment variable is empty.",
    )?;
    let directory = PathBuf::from(&values["directory"]);
    let bytes = io(tokio::fs::read(directory.join("ivy-ui.json")).await)?;
    let definition =
        parse(&String::from_utf8_lossy(&bytes), usize::MAX).map_err(|_| IvyError::internal())?;
    let client = HiveClient::new(
        &values["base"],
        ClientOptions {
            credential,
            ..ClientOptions::default()
        },
    )?;
    publish_ui(
        &client,
        PublishUiInput {
            directory: &directory,
            definition: &definition,
            mutation_id: &values["mutation-id"].as_str().into(),
            expected_release_id: values
                .get("expected-release")
                .map(|s| JsString::from(s.as_str()))
                .as_ref(),
            checked_bundle: None,
        },
    )
    .await
}
