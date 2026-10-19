use super::*;
use ivy_contracts::{checked_native_contract::native_versions, json::canonical};
use std::{path::PathBuf, process::Command};
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("secretary-package-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let actual = std::fs::canonicalize(&self.0).unwrap();
        assert_eq!(
            actual.parent(),
            Some(
                std::fs::canonicalize(std::env::temp_dir())
                    .unwrap()
                    .as_path()
            )
        );
        assert!(
            actual
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("secretary-package-")
        );
        if let Err(e) = std::fs::remove_dir_all(actual) {
            if !std::thread::panicking() {
                panic!("Package fixture cleanup: {e}")
            }
        }
    }
}
#[test]
fn retained_catalogs_match_original_installed_dist_layout_without_root_fallback() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let version = "0.159.2";
    assert!(native_versions().unwrap().contains(&version.into()));
    let catalog =
        std::fs::read(root.join(format!("specs/native/codex-{version}/catalog.json"))).unwrap();
    for kind in ["valid-dist", "missing-dist", "corrupt-dist"] {
        let artifact = Directory::new();
        let decoy = artifact.0.join(format!("specs/native/codex-{version}"));
        std::fs::create_dir_all(&decoy).unwrap();
        std::fs::write(decoy.join("catalog.json"), &catalog).unwrap();
        if kind != "missing-dist" {
            let directory = artifact
                .0
                .join(format!("dist/specs/native/codex-{version}"));
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join("catalog.json"),
                if kind == "corrupt-dist" {
                    b"{}".as_slice()
                } else {
                    catalog.as_slice()
                },
            )
            .unwrap();
        }
        let mut command = Command::new("node");
        command
            .arg("tests/secretary-rust/package-catalog-oracle.mjs")
            .arg(&artifact.0)
            .arg(version)
            .current_dir(&root);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected = parse(std::str::from_utf8(&output.stdout).unwrap(), 1024 * 1024).unwrap();
        let selected = ivy_secretary::service::retained_native_contracts(&artifact.0)
            .checked_native_contract(&version.into(), None);
        let actual = match selected {
            Ok(value) => IvyJson::object([
                ("catalogHash", value.catalog_hash().into()),
                (
                    "sourceHash",
                    value.catalog().required("sourceHash").unwrap().clone(),
                ),
                (
                    "nativeExecutableHash",
                    value
                        .catalog()
                        .required("nativeExecutableHash")
                        .unwrap()
                        .clone(),
                ),
            ]),
            Err(error) => IvyJson::object([
                ("code", IvyJson::String(error.code_js())),
                ("outcome", error.outcome.as_str().into()),
            ]),
        };
        assert_eq!(
            canonical(&actual, 1024 * 1024).unwrap(),
            canonical(&expected, 1024 * 1024).unwrap(),
            "{kind}"
        );
        if kind == "valid-dist" {
            assert!(actual.get("catalogHash").is_some());
        } else {
            assert!(actual.get("code").is_some());
        }
    }
    println!(
        "installed catalogs:3 exact original loader comparisons; dist-only success and missing/corrupt refusal despite a valid artifact-root decoy"
    );
}
