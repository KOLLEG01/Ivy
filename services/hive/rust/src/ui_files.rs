//! Private immutable UI files, staging, collection and format-4 legacy conversion.
use crate::{
    store::{Store, sql_error},
    uis::asset_path,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::digest,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_host_runtime::paths::absolute;
use rusqlite::{OptionalExtension, params};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

const DAY_MS: f64 = 86_400_000.0;
#[path = "ui_collection.rs"]
mod collection;
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn string<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    value
        .get(key)
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn component_id(value: &JsString) -> IvyResult<String> {
    require(
        !value.0.is_empty()
            && value.0.len() <= 128
            && (b'a' as u16..=b'z' as u16).contains(&value.0[0])
            && value.0.iter().all(|c| {
                (b'a' as u16..=b'z' as u16).contains(c)
                    || (b'0' as u16..=b'9' as u16).contains(c)
                    || *c == b'-' as u16
            }),
        "invalid_arguments",
        "UI id is invalid.",
    )?;
    Ok(text(value))
}
fn release_id(value: &JsString) -> IvyResult<String> {
    require(
        !value.0.is_empty()
            && value.0.len() <= 128
            && value.0.iter().all(|c| {
                *c < 128 && (*c as u8).is_ascii_alphanumeric()
                    || *c == b'_' as u16
                    || *c == b'-' as u16
            }),
        "invalid_arguments",
        "UI release id is invalid.",
    )?;
    Ok(text(value))
}
// UI storage uses the source's strict string prefix, rather than the host's
// case-insensitive path.relative policy. In particular the separator is required.
fn contained(directory: &Path, path: &Path) -> bool {
    let mut prefix = directory.as_os_str().to_os_string();
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
fn os_string(value: &OsStr) -> JsString {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        JsString(value.encode_wide().collect())
    }
    #[cfg(not(windows))]
    {
        value.to_string_lossy().into_owned().into()
    }
}
fn entries(path: &Path) -> IvyResult<Vec<fs::DirEntry>> {
    let mut values = fs::read_dir(path)
        .map_err(sql_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_error)?;
    values.sort_by_key(|entry| entry.file_name());
    Ok(values)
}
fn remove(path: &Path, recursive: bool) -> IvyResult<()> {
    let result = (|| -> io::Result<()> {
        let kind = fs::symlink_metadata(path)?.file_type();
        if kind.is_dir() && recursive {
            return fs::remove_dir_all(path);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileTypeExt;
            if kind.is_symlink_dir() {
                return fs::remove_dir(path);
            }
        }
        fs::remove_file(path)
    })();
    match result {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other.map_err(sql_error),
    }
}
fn matches_bytes(asset: &IvyJson, bytes: &[u8]) -> bool {
    asset.get("byteLength").and_then(IvyJson::as_f64) == Some(bytes.len() as f64)
        && asset.get("contentHash").and_then(IvyJson::as_js_string) == Some(&digest(bytes).into())
}
fn write_atomic(file: &Path, bytes: &[u8]) -> IvyResult<()> {
    use std::io::Write;
    let mut name = file.as_os_str().to_os_string();
    name.push(format!(".{}.partial", uuid::Uuid::new_v4()));
    let temporary = PathBuf::from(name);
    let result = (|| {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(sql_error)?
            .write_all(bytes)
            .map_err(sql_error)?;
        fs::rename(&temporary, file).map_err(sql_error)
    })();
    remove(&temporary, false)?;
    result
}
fn stale_partial(name: &JsString) -> bool {
    if name.0.len() != 44
        || !name
            .0
            .ends_with(&".partial".encode_utf16().collect::<Vec<_>>())
    {
        return false;
    }
    name.0[..36].iter().enumerate().all(|(i, c)| {
        if [8, 13, 18, 23].contains(&i) {
            *c == b'-' as u16
        } else {
            (b'0' as u16..=b'9' as u16).contains(c) || (b'a' as u16..=b'f' as u16).contains(c)
        }
    })
}

pub struct UiFiles {
    pub root: PathBuf,
}
impl UiFiles {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn directory(&self, ui: &JsString, release: &JsString) -> IvyResult<PathBuf> {
        let path = absolute(&self.root.join(component_id(ui)?).join(release_id(release)?))
            .map_err(sql_error)?;
        require(
            contained(&absolute(&self.root).map_err(sql_error)?, &path),
            "invalid_arguments",
            "UI directory is invalid.",
        )?;
        Ok(path)
    }
    pub fn path(
        &self,
        ui: &JsString,
        release: &JsString,
        relative: &JsString,
    ) -> IvyResult<PathBuf> {
        let directory = self.directory(ui, release)?;
        let path = absolute(&directory.join(text(asset_path(relative)?))).map_err(sql_error)?;
        require(
            contained(&directory, &path),
            "invalid_arguments",
            "UI asset path is invalid.",
        )?;
        Ok(path)
    }
    pub fn available(&self, ui: &JsString, release: &JsString, asset: &IvyJson) -> bool {
        (|| -> IvyResult<bool> {
            let file = self.path(ui, release, string(asset, "path")?)?;
            let directory = fs::canonicalize(self.directory(ui, release)?).map_err(sql_error)?;
            let info = fs::symlink_metadata(&file).map_err(sql_error)?;
            Ok(info.is_file()
                && contained(&directory, &fs::canonicalize(&file).map_err(sql_error)?)
                && asset.get("byteLength").and_then(IvyJson::as_f64) == Some(info.len() as f64))
        })()
        .unwrap_or(false)
    }
    pub fn check(&self, ui: &JsString, release: &JsString, asset: &IvyJson) -> IvyResult<bool> {
        if !self.available(ui, release, asset) {
            return Ok(false);
        }
        Ok(asset.get("contentHash").and_then(IvyJson::as_js_string)
            == Some(
                &digest(
                    fs::read(self.path(ui, release, string(asset, "path")?)?).map_err(sql_error)?,
                )
                .into(),
            ))
    }
    fn staging_directory(&self, ui: &JsString, release: &JsString) -> IvyResult<PathBuf> {
        absolute(
            &self
                .root
                .join(".staging")
                .join(component_id(ui)?)
                .join(release_id(release)?),
        )
        .map_err(sql_error)
    }
    pub fn stage(
        &self,
        ui: &JsString,
        release: &JsString,
        asset: &IvyJson,
        bytes: &[u8],
    ) -> IvyResult<()> {
        if asset.is_null() {
            return Err(IvyError::internal());
        }
        require(
            matches_bytes(asset, bytes),
            "artifact_changed",
            "Staged UI file differs from its descriptor.",
        )?;
        let directory = self.staging_directory(ui, release)?;
        let file = absolute(&directory.join(text(asset_path(string(asset, "path")?)?)))
            .map_err(sql_error)?;
        require(
            contained(&directory, &file),
            "invalid_arguments",
            "UI asset path is invalid.",
        )?;
        fs::create_dir_all(file.parent().ok_or_else(IvyError::internal)?).map_err(sql_error)?;
        if file.exists() {
            return require(
                asset.get("contentHash").and_then(IvyJson::as_js_string)
                    == Some(&digest(fs::read(&file).map_err(sql_error)?).into()),
                "mutation_conflict",
                "Staged UI file already has other content.",
            );
        }
        write_atomic(&file, bytes)
    }
    pub fn promote(&self, ui: &JsString, release: &JsString, assets: &[IvyJson]) -> IvyResult<()> {
        let mut complete = true;
        for asset in assets {
            if !self.check(ui, release, asset)? {
                complete = false;
                break;
            }
        }
        if complete {
            return Ok(());
        }
        let source = self.staging_directory(ui, release)?;
        for asset in assets {
            let file = absolute(&source.join(text(asset_path(string(asset, "path")?)?)))
                .map_err(sql_error)?;
            require(
                contained(&source, &file)
                    && file.exists()
                    && asset.get("contentHash").and_then(IvyJson::as_js_string)
                        == Some(&digest(fs::read(&file).map_err(sql_error)?).into())
                    && asset.get("byteLength").and_then(IvyJson::as_f64)
                        == Some(fs::metadata(&file).map_err(sql_error)?.len() as f64),
                "ui_release_invalid",
                "Staged UI release is incomplete.",
            )?;
        }
        let target = self.directory(ui, release)?;
        require(
            !target.exists(),
            "ui_release_invalid",
            "Existing UI release files are incomplete.",
        )?;
        fs::create_dir_all(target.parent().ok_or_else(IvyError::internal)?).map_err(sql_error)?;
        fs::rename(source, target).map_err(sql_error)
    }
    pub async fn read(
        &self,
        ui: &JsString,
        release: &JsString,
        asset: &IvyJson,
    ) -> IvyResult<Vec<u8>> {
        let file = self.path(ui, release, string(asset, "path")?)?;
        let directory = tokio::fs::canonicalize(self.directory(ui, release)?)
            .await
            .map_err(sql_error)?;
        require(
            tokio::fs::symlink_metadata(&file)
                .await
                .map_err(sql_error)?
                .is_file()
                && contained(
                    &directory,
                    &tokio::fs::canonicalize(&file).await.map_err(sql_error)?,
                ),
            "ui_release_invalid",
            "UI asset leaves its private release directory.",
        )?;
        let bytes = tokio::fs::read(&file).await.map_err(sql_error)?;
        require(
            matches_bytes(asset, &bytes),
            "ui_release_invalid",
            "UI asset differs from its release manifest.",
        )?;
        Ok(bytes)
    }
    pub async fn install(
        &self,
        source: &Path,
        ui: &JsString,
        release: &JsString,
        assets: &[IvyJson],
    ) -> IvyResult<()> {
        use tokio::io::AsyncWriteExt;
        let target = self.directory(ui, release)?;
        let mut complete = true;
        for asset in assets {
            if !self.check(ui, release, asset)? {
                complete = false;
                break;
            }
        }
        if complete {
            return Ok(());
        }
        require(
            !target.exists(),
            "ui_release_invalid",
            "Existing UI release files are incomplete.",
        )?;
        let incoming = absolute(&self.root.join(".incoming")).map_err(sql_error)?;
        let staging = incoming.join(format!(
            "{}-{}-{}",
            component_id(ui)?,
            release_id(release)?,
            uuid::Uuid::new_v4()
        ));
        require(
            contained(&incoming, &staging),
            "invalid_arguments",
            "UI staging directory is invalid.",
        )?;
        tokio::fs::create_dir_all(&staging)
            .await
            .map_err(sql_error)?;
        let result = async {
            for asset in assets {
                let relative = text(asset_path(string(asset, "path")?)?);
                let file = absolute(&staging.join(&relative)).map_err(sql_error)?;
                require(
                    contained(&staging, &file),
                    "invalid_arguments",
                    "UI asset path is invalid.",
                )?;
                let mut bytes =
                    tokio::fs::read(absolute(&source.join(relative)).map_err(sql_error)?)
                        .await
                        .map_err(sql_error)?;
                if asset.get("mediaType").and_then(IvyJson::as_js_string)
                    == Some(&"application/json".into())
                {
                    bytes = canonical(
                        &parse(&String::from_utf8_lossy(&bytes), usize::MAX).map_err(sql_error)?,
                        MANAGEMENT_FRAME_BYTES,
                    )?
                    .into_bytes();
                }
                require(
                    matches_bytes(asset, &bytes),
                    "artifact_changed",
                    "UI bundle changed after validation.",
                )?;
                tokio::fs::create_dir_all(file.parent().ok_or_else(IvyError::internal)?)
                    .await
                    .map_err(sql_error)?;
                tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&file)
                    .await
                    .map_err(sql_error)?
                    .write_all(&bytes)
                    .await
                    .map_err(sql_error)?;
            }
            tokio::fs::create_dir_all(target.parent().ok_or_else(IvyError::internal)?)
                .await
                .map_err(sql_error)?;
            tokio::fs::rename(&staging, target).await.map_err(sql_error)
        }
        .await;
        // Keep finally semantics: cleanup failure supersedes the operation's error.
        match tokio::fs::remove_dir_all(staging).await {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(sql_error(e)),
        }
        result
    }
    pub fn migrate_legacy(&self, store: &mut Store) -> IvyResult<()> {
        if store.metadata("ui_files_v1")?.as_deref() == Some("ready") {
            return Ok(());
        }
        // Private UI storage requires an explicit root.
        if self.root.as_os_str().is_empty() {
            return Err(IvyError::internal());
        }
        fs::create_dir_all(&self.root).map_err(sql_error)?;
        let rows = store
            .connection()
            .prepare("SELECT app_id,release_id,release_json FROM app_releases")
            .map_err(sql_error)?
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(sql_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_error)?;
        let mut updates = vec![];
        for (ui, id, encoded) in rows {
            let Ok(mut release) = parse(&encoded, usize::MAX) else {
                continue;
            };
            let Some(old_assets) = release.get("assets").and_then(IvyJson::as_array) else {
                continue;
            };
            let mut legacy = false;
            for asset in old_assets {
                if asset.is_null() {
                    return Err(IvyError::internal());
                }
                if asset
                    .get("objectId")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|v| !v.0.is_empty())
                {
                    legacy = true;
                    break;
                }
            }
            if !legacy {
                continue;
            }
            let ui: JsString = ui.into();
            let id: JsString = id.into();
            let target = self.directory(&ui, &id)?;
            let mut assets = vec![];
            for old in old_assets {
                if old.is_null() {
                    return Err(IvyError::internal());
                }
                let revision = old.get("revision").and_then(IvyJson::as_f64);
                require(
                    old.get("objectId")
                        .and_then(IvyJson::as_js_string)
                        .is_some_and(|v| !v.0.is_empty())
                        && revision.is_some_and(|v| {
                            v.is_finite() && v.fract() == 0.0 && v.abs() <= 9_007_199_254_740_991.0
                        }),
                    "storage_invalid",
                    "Legacy UI asset is invalid.",
                )?;
                let stored = store.connection().query_row(
                    "SELECT r.content,r.encoding,r.content_hash,r.byte_length,c.media_type FROM revisions r JOIN contract_families c ON c.key=r.contract_key WHERE r.object_id=? AND r.revision=?",
                    params![text(string(old,"objectId")?), revision.unwrap()],
                    |r| {
                        // Node's SQLite binding decodes TEXT with UTF-8 replacement
                        // before Buffer.from(String(content)) verifies these bytes.
                        let content=match r.get_ref(0)? {
                            rusqlite::types::ValueRef::Text(bytes)=>rusqlite::types::Value::Text(String::from_utf8_lossy(bytes).into_owned()),
                            _=>r.get::<_,rusqlite::types::Value>(0)?,
                        };
                        Ok((content,r.get::<_, String>(1)?,r.get::<_, String>(2)?,r.get::<_, i64>(3)?,r.get::<_, String>(4)?))
                    })
                    .optional().map_err(sql_error)?;
                let valid = stored.as_ref().is_some_and(|r| {
                    old.get("contentHash").and_then(IvyJson::as_js_string)
                        == Some(&r.2.clone().into())
                        && old.get("mediaType").and_then(IvyJson::as_js_string)
                            == Some(&r.4.clone().into())
                });
                require(
                    valid,
                    "storage_invalid",
                    "Legacy UI asset is missing or changed.",
                )?;
                let (content, encoding, hash, length, media) = stored.unwrap();
                let bytes = match content {
                    rusqlite::types::Value::Blob(v) if encoding == "base64" => v,
                    rusqlite::types::Value::Text(v) if encoding != "base64" => v.into_bytes(),
                    _ => return Err(IvyError::internal()),
                };
                require(
                    digest(&bytes) == hash && bytes.len() as i64 == length,
                    "storage_invalid",
                    "Legacy UI asset checksum is invalid.",
                )?;
                let relative = asset_path(string(old, "path")?)?;
                let asset = IvyJson::object([
                    ("path", IvyJson::String(relative.clone())),
                    ("mediaType", media.into()),
                    ("contentHash", hash.into()),
                    ("byteLength", IvyJson::Number(bytes.len() as f64)),
                ]);
                let file = self.path(&ui, &id, relative)?;
                fs::create_dir_all(file.parent().ok_or_else(IvyError::internal)?)
                    .map_err(sql_error)?;
                if file.exists() {
                    require(
                        self.check(&ui, &id, &asset)?,
                        "storage_invalid",
                        "Existing migrated UI asset differs.",
                    )?;
                } else {
                    write_atomic(&file, &bytes)?;
                }
                let mut prefix = os_string(file.file_name().ok_or_else(IvyError::internal)?).0;
                prefix.push(b'.' as u16);
                for entry in entries(file.parent().ok_or_else(IvyError::internal)?)? {
                    let name = os_string(&entry.file_name());
                    if entry.file_type().map_err(sql_error)?.is_file()
                        && name.0.starts_with(&prefix)
                        && stale_partial(&JsString(name.0[prefix.len()..].to_vec()))
                    {
                        fs::remove_file(entry.path()).map_err(sql_error)?;
                    }
                }
                assets.push(asset);
            }
            let mut complete = true;
            for asset in &assets {
                if !self.check(&ui, &id, asset)? {
                    complete = false;
                    break;
                }
            }
            require(
                complete && target.exists(),
                "storage_invalid",
                "Migrated UI release is incomplete.",
            )?;
            release.insert("assets", IvyJson::Array(assets));
            updates.push((
                text(&ui),
                text(&id),
                canonical(&release, MANAGEMENT_FRAME_BYTES)?,
            ));
        }
        store.transaction(|store| {
            for (ui,id,json) in updates { store.connection().execute("UPDATE app_releases SET release_json=? WHERE app_id=? AND release_id=?",params![json,ui,id]).map_err(sql_error)?; }
            store.connection().execute("DELETE FROM app_assets",[]).map_err(sql_error)?;
            let objects = store.connection().prepare("SELECT id,contract_key FROM objects WHERE contract_key LIKE 'ui/asset-%' ORDER BY contract_key").map_err(sql_error)?
                .query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(sql_error)?.collect::<Result<Vec<_>,_>>().map_err(sql_error)?;
            for (id,key) in objects.iter().filter(|(_,k)|k != "ui/asset-owner").chain(objects.iter().filter(|(_,k)|k == "ui/asset-owner")) {
                let (revisions,bytes) = store.connection().query_row("SELECT COUNT(*),COALESCE(SUM(byte_length),0) FROM revisions WHERE object_id=?",[id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).map_err(sql_error)?;
                for table in ["object_fts","revisions","objects"] {
                    store.connection().execute(&format!("DELETE FROM {table} WHERE {}=?",if table=="objects"{"id"}else{"object_id"}),[id]).map_err(sql_error)?;
                }
                store.adjust_retention_usage(&key.clone().into(),-1,-revisions,-bytes)?;
            }
            store.set_metadata("ui_files_v1","ready")
        })
    }
}
