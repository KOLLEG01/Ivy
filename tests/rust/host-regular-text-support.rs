use super::native_path;
use ivy_contracts::JsString;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub struct Fixture {
    pub root: PathBuf,
    base: PathBuf,
}
impl Fixture {
    pub fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = base.join(format!("ivy-regular-text-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self { root, base }
    }
    pub fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
    pub fn reset(&self) {
        for name in ["input", "input.held"] {
            let path = self.path(name);
            if let Ok(meta) = fs::symlink_metadata(&path) {
                assert!(!meta.is_dir());
                fs::remove_file(path).unwrap();
            }
        }
        fs::write(self.path("input"), b"retained").unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert_eq!(self.root.parent(), Some(self.base.as_path()));
        assert!(
            self.root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-regular-text-")
        );
        let removed = fs::remove_dir_all(&self.root);
        if !std::thread::panicking() {
            removed.unwrap();
        }
    }
}
pub fn text(path: &Path) -> JsString {
    native_path::from_native(path)
}
