#[path = "../../packages/host-runtime/rust/src/paths.rs"]
mod paths;
use std::{fs, path::PathBuf};

#[test]
fn containment_normalizes_parents_and_rejects_sibling_prefixes() {
    let root = std::env::temp_dir().join("ivy-path-test");
    assert!(paths::inside(&root, &root.join("a/../b")).unwrap());
    assert!(paths::inside(&root, &root).unwrap());
    assert!(!paths::inside(&root, &root.join("../other")).unwrap());
    assert!(!paths::inside(&root, &PathBuf::from(format!("{}-sibling", root.display()))).unwrap());
}

#[test]
fn future_path_uses_the_existing_ancestor_without_creating_directories() {
    let root = std::env::temp_dir().join(format!("ivy-future-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let expected = fs::canonicalize(&root).unwrap().join("missing/leaf");
    assert_eq!(paths::resolved_future_path(&root.join("missing/leaf")).unwrap(), expected);
    assert!(!root.join("missing").exists());
    fs::remove_dir(&root).unwrap();
}

#[cfg(windows)]
#[test]
fn windows_containment_ignores_case() {
    assert!(paths::inside(std::path::Path::new("C:\\Local\\Owned"), std::path::Path::new("c:\\local\\owned\\child")).unwrap());
}
