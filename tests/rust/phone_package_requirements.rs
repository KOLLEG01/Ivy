use super::*;
use crate::native_plan::materialize_native_plan;
use ivy_contracts::json::parse;
use std::{fs, path::PathBuf};

#[test]
fn native_phone_requires_rust_files_and_legacy_keeps_its_input_dll() {
    let source = parse(
        include_str!("../../services/phone-bridge/deploy.json"),
        65536,
    )
    .unwrap();
    let mut native = materialize_native_plan(source, "win32", "x64").unwrap();
    assert_eq!(
        portable_native_files_for_manifest(&native).unwrap(),
        [
            "dist/native/win32-x64/ivy-job.exe",
            "dist/native/win32-x64/ivy-phone-native.exe",
            "dist/native/win32-x64/ivy_phone_evs.dll",
        ]
    );
    let mut entry = native.required("entrypoint").unwrap().clone();
    entry.insert("executable", "node".into());
    native.insert("entrypoint", entry);
    assert_eq!(
        portable_native_files_for_manifest(&native).unwrap(),
        ["dist/native/ivy-job.exe", "dist/native/ivy-phone-input.dll",]
    );
}

struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        assert!(
            self.0.starts_with(std::env::temp_dir())
                && self
                    .0
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("ivy-phone-package-requirements-")
        );
        fs::remove_dir_all(&self.0).unwrap();
    }
}
#[test]
fn native_helper_selection_never_falls_back_to_another_layout() {
    let root = Root(std::env::temp_dir().join(format!(
        "ivy-phone-package-requirements-{}",
        uuid::Uuid::new_v4()
    )));
    fs::create_dir_all(root.0.join("dist/native")).unwrap();
    let legacy = root.0.join("dist/native/ivy-job.exe");
    fs::write(&legacy, b"legacy fixture; never executed").unwrap();
    assert_eq!(
        component_native_helper(&root.0, "ivy-job.exe", NativeHelperLayout::Legacy).unwrap(),
        crate::paths::realpath(&legacy).unwrap()
    );
    assert_eq!(
        component_native_helper(&root.0, "ivy-job.exe", NativeHelperLayout::Target)
            .unwrap_err()
            .code,
        "artifact_missing"
    );
    let selected = root
        .0
        .join("dist/native")
        .join(native_platform().unwrap())
        .join("ivy-job.exe");
    fs::create_dir_all(selected.parent().unwrap()).unwrap();
    fs::write(&selected, b"target fixture; never executed").unwrap();
    assert_eq!(
        component_native_helper(&root.0, "ivy-job.exe", NativeHelperLayout::Target).unwrap(),
        crate::paths::realpath(&selected).unwrap()
    );
    assert_eq!(
        component_native_helper(&root.0, "../ivy-job.exe", NativeHelperLayout::Target)
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
}
