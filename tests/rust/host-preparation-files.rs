use ivy_contracts::IvyJson;
use std::{fs, path::PathBuf};

#[path = "host-preparation-files-cases.rs"]
mod preparation_files_cases;

struct ServiceFixture(PathBuf);
impl ServiceFixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ivy-preparation-files-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap(); Self(root)
    }
}
impl Drop for ServiceFixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn assert_json_eq(actual: &IvyJson, expected: &IvyJson) {
    assert_eq!(ivy_contracts::json::canonical(actual, usize::MAX).unwrap(), ivy_contracts::json::canonical(expected, usize::MAX).unwrap());
}
