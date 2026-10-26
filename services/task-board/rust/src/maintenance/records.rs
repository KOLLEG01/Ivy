use ivy_contracts::{IvyError, IvyResult};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectPin {
    pub object_id: String,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveManifest {
    pub host_id: String,
    pub path: String,
    pub content_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedAttachment {
    pub object: ObjectPin,
    pub content_hash: String,
    pub byte_length: u64,
    pub local_path: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedAttachment {
    #[serde(flatten)]
    pub archive: ArchivedAttachment,
    pub owner_task_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedPage {
    #[serde(flatten)]
    pub object: ObjectPin,
    pub owner_task_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactPlan {
    pub schema_version: u8,
    pub host_id: String,
    pub service_node_id: String,
    pub scope: String,
    pub archive_manifest: ArchiveManifest,
    pub task_ids: Vec<String>,
    pub files: Vec<PlannedAttachment>,
    pub reference_pages: Vec<PlannedPage>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactJob {
    pub schema_version: u8,
    pub scope: String,
    pub archive_manifest: ArchiveManifest,
    pub task_ids: Vec<String>,
    pub reference_pages: Vec<ObjectPin>,
    pub remaining: Vec<ArchivedAttachment>,
    pub deleted_files: u64,
    pub deleted_bytes: u64,
    pub pruned_revisions: u64,
    #[serde(default)]
    pub prune_attempts: u64,
    #[serde(default)]
    pub task_cursor: usize,
    #[serde(default)]
    pub file_cursor: usize,
    #[serde(default)]
    pub run_history_cursor: Option<String>,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// The verified plan is saved before references change. A restart resumes
    /// even if the operator lost its reply before queueing the retired files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retire_pending: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedCommentBody {
    pub comment_id: String,
    pub body_hash: String,
    pub local_path: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentTask {
    pub task_id: String,
    pub comments: Vec<ArchivedCommentBody>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentJob {
    pub schema_version: u8,
    pub scope: String,
    pub archive_manifest: ArchiveManifest,
    pub remaining: Vec<CommentTask>,
    pub completed_tasks: u64,
    pub rewritten_comments: u64,
    pub removed_characters: i64,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

pub(super) fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
pub(super) fn hash(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value.as_bytes()[7..]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
}
pub(super) fn absolute(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.starts_with('/')
        || bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\')
}
pub(super) fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
pub async fn load<T: DeserializeOwned + Send + 'static>(path: PathBuf) -> IvyResult<Option<T>> {
    tokio::task::spawn_blocking(move || match fs::File::open(path) {
        Ok(file) => serde_json::from_reader(file).map(Some).map_err(|_| {
            IvyError::new(
                "invalid_arguments",
                "Invalid TaskBoard maintenance document.",
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(IvyError::new(
            "storage_unavailable",
            "TaskBoard maintenance document cannot be read.",
        )),
    })
    .await
    .map_err(|_| IvyError::internal())?
}
pub async fn save<T: Serialize>(path: &Path, value: &T) -> IvyResult<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| IvyError::internal())?;
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || ivy_store::atomic::atomic_write(&path, &bytes))
        .await
        .map_err(|_| IvyError::internal())?
        .map_err(|_| {
            IvyError::new(
                "storage_unavailable",
                "TaskBoard maintenance progress could not be saved.",
            )
        })
}
