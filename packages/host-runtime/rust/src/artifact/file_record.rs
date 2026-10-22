//! Canonical file observation shared by artifact inventory and backup/restore.
use crate::{IvyJson, IvyResult};
use ivy_contracts::{JsString, hash::hash_json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactFile {
    pub path: JsString,
    pub hash: String,
    pub bytes: u64,
    pub mode: u32,
}
impl ArtifactFile {
    pub fn json(&self) -> IvyJson {
        let mut value = self.identity();
        value.insert("mode", IvyJson::Number(self.mode as f64));
        value
    }
    fn identity(&self) -> IvyJson {
        IvyJson::object([
            ("path", IvyJson::String(self.path.clone())),
            ("hash", self.hash.clone().into()),
            ("bytes", IvyJson::Number(self.bytes as f64)),
        ])
    }
}

/// Candidate source identity deliberately excludes checkout permission bits.
pub fn artifact_source_identity(files: &[ArtifactFile]) -> IvyResult<String> {
    hash_json(&IvyJson::Array(
        files.iter().map(ArtifactFile::identity).collect(),
    ))
}
