//! Message attachments retain the original cross-platform filename policy.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, agent_schema::validate_agent, hash::digest,
};
use ivy_host_runtime::{
    filesystem,
    node_path::{self, Style},
};
use ivy_sdk::service::ToolHandler;
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

pub const MAXIMUM_BYTES: usize = 8 * 1024 * 1024;
pub const RETENTION_MS: f64 = 30.0 * 24.0 * 60.0 * 60.0 * 1000.0;

/// The shared runtime supplies a Rust codec which accepts only canonical
/// standard base64. Invalid/noncanonical input returns None, including Unicode.
/// There is no production codec fallback in this domain owner.
pub type CanonicalBase64Decoder = dyn Fn(&JsString) -> Option<Vec<u8>> + Send + Sync;
pub type StagedClock = dyn Fn() -> f64 + Send + Sync;

pub struct StagedFiles {
    pub root: JsString,
    pub decode: Arc<CanonicalBase64Decoder>,
    pub now: Arc<StagedClock>,
}
impl StagedFiles {
    pub fn new(root: JsString, decode: Arc<CanonicalBase64Decoder>) -> Self {
        Self {
            root,
            decode,
            now: Arc::new(|| chrono::Utc::now().timestamp_millis() as f64),
        }
    }
    pub fn handler(self: &Arc<Self>) -> ToolHandler {
        let owner = self.clone();
        Arc::new(move |args, _| {
            let owner = owner.clone();
            Box::pin(async move {
                validate_agent("StageFileInput", &args)?;
                owner
                    .stage(
                        args.required("name")?.as_js_string().unwrap(),
                        args.required("dataBase64")?.as_js_string().unwrap(),
                    )
                    .await
            })
        })
    }
    pub async fn stage(&self, name: &JsString, encoded: &JsString) -> IvyResult<IvyJson> {
        let now = (self.now)();
        let bytes = (self.decode)(encoded)
            .filter(|bytes| bytes.len() <= MAXIMUM_BYTES)
            .ok_or_else(|| {
                IvyError::new(
                    "invalid_arguments",
                    "A staged file must be canonical base64 up to 8 MiB.",
                )
            })?;
        let hash = digest(&bytes);
        let name = staged_name(name);
        let directory = node_path::join(Style::native(), &[self.root.clone(), hash[7..].into()]);
        let path = node_path::join(Style::native(), &[directory.clone(), name.clone()]);
        let length = bytes.len();
        tokio::fs::create_dir_all(node_path::filesystem_path(&directory))
            .await
            .map_err(|_| IvyError::internal())?;
        tokio::fs::write(node_path::filesystem_path(&path), bytes)
            .await
            .map_err(|_| IvyError::internal())?;
        // Date clips to integer milliseconds, but only after the original file
        // write. Invalid caller clocks retain that same partial-write boundary.
        let time = if now.is_finite() && now.abs() <= 8_640_000_000_000_000.0 {
            now.trunc() as i64
        } else {
            return Err(IvyError::internal());
        };
        tokio::task::spawn_blocking(move || {
            filesystem::set_times_ms(&node_path::to_native(&directory), time, time)
        })
        .await
        .map_err(|_| IvyError::internal())?
        .map_err(|_| IvyError::internal())?;
        let mut entries = tokio::fs::read_dir(node_path::filesystem_path(&self.root))
            .await
            .map_err(|_| IvyError::internal())?;
        let mut directories = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|_| IvyError::internal())?
        {
            if entry
                .file_type()
                .await
                .map_err(|_| IvyError::internal())?
                .is_dir()
            {
                directories.push(node_path::join(
                    Style::native(),
                    &[self.root.clone(), node_path::node_utf8(&entry.file_name())],
                ));
            }
        }
        // Snapshot enumeration before pruning, like readdir(withFileTypes).
        for directory in directories {
            let modified = tokio::fs::metadata(node_path::filesystem_path(&directory))
                .await
                .map_err(|_| IvyError::internal())?
                .modified()
                .map_err(|_| IvyError::internal())?;
            let modified = milliseconds(modified);
            if now - modified > RETENTION_MS {
                filesystem::remove_force(node_path::to_native(&directory)).await?;
            }
        }
        Ok(IvyJson::object([
            ("path", IvyJson::String(path)),
            ("name", IvyJson::String(name)),
            ("byteLength", IvyJson::Number(length as f64)),
            ("contentHash", hash.into()),
        ]))
    }
}
fn milliseconds(time: SystemTime) -> f64 {
    let value = |duration: std::time::Duration| {
        duration.as_secs() as f64 * 1000.0 + duration.subsec_nanos() as f64 / 1_000_000.0
    };
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => value(duration),
        Err(error) => -value(error.duration()),
    }
}

/// One path segment, keeping reserved names within the published UTF-16 cap.
pub fn staged_name(name: &JsString) -> JsString {
    let last = name
        .0
        .rsplit(|unit| matches!(*unit, 47 | 92))
        .next()
        .unwrap_or(&[]);
    let safe: Vec<_> = last
        .iter()
        .map(|unit| {
            if matches!(*unit, 0..=31 | 34 | 42 | 58 | 60 | 62 | 63 | 124) {
                95
            } else {
                *unit
            }
        })
        .collect();
    // The source class is the union of '.' and ECMAScript whitespace. Retain
    // the shared trim boundary, including BOM and excluding NEL.
    let edge = |unit: &u16| *unit == 46 || JsString(vec![*unit]).trim().0.is_empty();
    let start = safe
        .iter()
        .position(|unit| !edge(unit))
        .unwrap_or(safe.len());
    let end = safe
        .iter()
        .rposition(|unit| !edge(unit))
        .map_or(start, |index| index + 1);
    let mut units = safe[start..end]
        .iter()
        .take(128)
        .copied()
        .collect::<Vec<_>>();
    if units.is_empty() {
        return "file".into();
    }
    let first: Vec<_> = units
        .iter()
        .copied()
        .take_while(|unit| *unit != 46)
        .map(|unit| {
            if (65..=90).contains(&unit) {
                unit + 32
            } else {
                unit
            }
        })
        .collect();
    let reserved = ["con", "prn", "aux", "nul"]
        .iter()
        .any(|name| first.iter().copied().eq(name.encode_utf16()))
        || first.len() == 4
            && (first.starts_with(&[99, 111, 109]) || first.starts_with(&[108, 112, 116]))
            && (48..=57).contains(&first[3]);
    if reserved {
        units.insert(0, 95);
        units.truncate(128);
    }
    JsString(units)
}
