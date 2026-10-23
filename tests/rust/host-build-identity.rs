use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use ivy_host_runtime::build_identity::build_identity;
use std::{collections::BTreeSet, path::PathBuf, process::Command};

#[test]
#[ignore = "isolated environment entrypoint"]
fn identity_child() {
    let root = PathBuf::from(std::env::var_os("IVY_IDENTITY_TEST_ROOT").unwrap());
    let result = match build_identity(&root) {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value.json())]),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    };
    println!(
        "IDENTITY_RESULT={}",
        stringify(&result, usize::MAX).unwrap()
    );
}

#[test]
fn native_build_identity_validates_metadata_and_preserves_supplied_ids_and_utf16() {
    let root = std::env::temp_dir().join(format!("ivy-build-identity-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let _cleanup = Cleanup(root.clone());
    let supplied = format!("sha256:{}", "a".repeat(64));
    let mut generated = BTreeSet::new();
    for (index, (body, valid)) in [
        (r#"{"version":"1.2.3"}"#, true),
        (r#"{"version":"\ud800"}"#, true),
        ("{}", true),
        ("null", false),
        ("[]", false),
        ("false", false),
        (r#"{"version":null}"#, false),
        (r#"{"version":1}"#, false),
        (r#"{"version":true}"#, false),
        (r#"{"version":["1.2.3"]}"#, false),
        (r#"{"version":{"toString":"1.2.3"}}"#, false),
    ]
    .into_iter()
    .enumerate()
    {
        let directory = root.join(index.to_string());
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("package.json"), body).unwrap();
        for id in [None, Some(supplied.as_str()), Some("invalid")] {
            let mut child = Command::new(std::env::current_exe().unwrap());
            child
                .args(["--ignored", "--exact", "identity_child", "--nocapture"])
                .env("IVY_IDENTITY_TEST_ROOT", &directory)
                .env_remove("IVY_BUILD_ID");
            if let Some(id) = id {
                child.env("IVY_BUILD_ID", id);
            }
            let result = child.output().unwrap();
            assert!(result.status.success());
            let stdout = String::from_utf8(result.stdout).unwrap();
            let value = parse(
                stdout
                    .lines()
                    .find_map(|line| line.strip_prefix("IDENTITY_RESULT="))
                    .unwrap(),
                usize::MAX,
            )
            .unwrap();
            assert_eq!(value.get("ok"), Some(&IvyJson::Bool(valid)), "{body}");
            if !valid {
                continue;
            }
            let value = value.required("value").unwrap();
            assert_eq!(
                value.get("version"),
                parse(body, usize::MAX).unwrap().get("version")
            );
            let actual = value.required("buildId").unwrap().as_str().unwrap();
            if id == Some(supplied.as_str()) {
                assert_eq!(actual, supplied);
            } else {
                assert!(actual.starts_with("sha256:") && actual.len() == 71);
                assert!(
                    actual[7..]
                        .bytes()
                        .all(|value| value.is_ascii_digit() || (b'a'..=b'f').contains(&value))
                );
                assert!(generated.insert(actual));
            }
        }
    }
    assert!(build_identity(&root.join("missing")).is_err());
    std::fs::create_dir(root.join("package.json")).unwrap();
    assert!(build_identity(&root).is_err());
}
