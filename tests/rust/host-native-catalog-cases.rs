use super::{Fixture, text};
use ivy_contracts::{
    IvyError, IvyJson, JsString,
    checked_native_contract::{native_catalog_path, native_versions},
    json::stringify,
};
use ivy_host_runtime::{
    native_catalog_compatibility::verify_configured_native_catalog, native_path,
};
use std::{ffi::OsString, fs, path::Path};

fn catalog(version: &JsString, executable: &JsString) -> Vec<u8> {
    stringify(
        &IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("provider", "codex".into()),
            ("version", IvyJson::String(version.clone())),
            ("experimental", true.into()),
            ("sourceHash", format!("sha256:{}", "11".repeat(32)).into()),
            ("nativeExecutableHash", IvyJson::String(executable.clone())),
            ("clientRequests", IvyJson::Array(vec![])),
            ("serverRequests", IvyJson::Array(vec![])),
            ("serverNotifications", IvyJson::Array(vec![])),
            ("clientNotifications", IvyJson::Array(vec![])),
            ("derivedLegacyTypes", IvyJson::Array(vec![])),
        ]),
        usize::MAX,
    )
    .unwrap()
    .into_bytes()
}

#[tokio::test]
async fn configured_catalog_reads_native_files_and_preserves_selection_and_refusals() {
    let fixture = Fixture::new();
    let version = &native_versions().unwrap()[0];
    let executable: JsString = format!("sha256:{}", "22".repeat(32)).into();
    let platform_executable: JsString = format!("sha256:{}", "33".repeat(32)).into();
    let fallback_bytes = catalog(version, &executable);
    let platform_bytes = catalog(version, &platform_executable);
    let mut names = vec![OsString::from("space; quote' 日本語 e\u{301} 😀")];
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        names.extend([0xD800, 0xDFFF].map(|unit| OsString::from_wide(&[b'n' as u16, unit])));
    }
    for (index, name) in names.into_iter().enumerate() {
        let parent = fixture.0.join(index.to_string());
        let root = parent.join(&name);
        let instance = text(Path::new(&name));
        let dist = root.join("dist");
        let fallback = native_path::to_native(
            &native_catalog_path(&text(&dist), version, None, None).unwrap(),
        );
        fs::create_dir_all(fallback.parent().unwrap()).unwrap();
        fs::create_dir(root.join("segment")).unwrap();
        let settings = |hash: &JsString| {
            IvyJson::object([
                ("nativeVersion", IvyJson::String(version.clone())),
                ("nativeExecutableHash", IvyJson::String(hash.clone())),
            ])
        };

        // A different, existing filename must not supply a missing native file.
        let alias = parent.join(name.to_string_lossy().as_ref());
        let alias_file = alias.join(fallback.strip_prefix(&root).unwrap());
        if alias != root {
            fs::create_dir_all(alias_file.parent().unwrap()).unwrap();
            fs::write(&alias_file, &fallback_bytes).unwrap();
        }
        assert_eq!(
            verify_configured_native_catalog(&root, &instance, &settings(&executable))
                .await
                .unwrap_err()
                .code,
            "internal_error"
        );
        fs::write(&fallback, &fallback_bytes).unwrap();
        if alias != root {
            fs::write(&alias_file, &platform_bytes).unwrap();
        }
        for input in [&root, &root.join("segment/..")] {
            verify_configured_native_catalog(input, &instance, &settings(&executable))
                .await
                .unwrap();
        }

        // Supply the current target's file while retaining the fallback catalog.
        let platform = match std::env::consts::OS {
            "windows" => "win32",
            "macos" => "darwin",
            name => name,
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            "x86" => "ia32",
            name => name,
        };
        let selected = fallback.with_file_name(format!("catalog.{platform}-{arch}.json"));
        fs::write(&selected, &platform_bytes).unwrap();
        let checked = native_catalog_path(&text(&dist), version, None, None).unwrap();
        assert_eq!(native_path::to_native(&checked), selected);
        verify_configured_native_catalog(&root, &instance, &settings(&platform_executable))
            .await
            .unwrap();

        let error = verify_configured_native_catalog(&root, &instance, &settings(&executable))
            .await
            .unwrap_err();
        let message = JsString(
            [
                instance.clone(),
                " is configured for native version ".into(),
                version.clone(),
                ", executable ".into(),
                executable.clone(),
                ", but the candidate requires executable ".into(),
                platform_executable.clone(),
                ". Prepare the matching native runtime and configuration before retrying.".into(),
            ]
            .into_iter()
            .flat_map(|part| part.0)
            .collect(),
        );
        assert_eq!(
            error,
            IvyError::new_js("incompatible_native_catalog".into(), message)
        );
        assert_eq!(fs::read(&fallback).unwrap(), fallback_bytes);
        assert_eq!(fs::read(&selected).unwrap(), platform_bytes);
        if alias != root {
            assert_eq!(fs::read(&alias_file).unwrap(), platform_bytes);
        }

        fs::write(
            &selected,
            catalog(&"different-version".into(), &platform_executable),
        )
        .unwrap();
        assert_eq!(
            verify_configured_native_catalog(&root, &instance, &settings(&platform_executable))
                .await
                .unwrap_err()
                .code,
            "incompatible_native_catalog"
        );
        fs::remove_file(&selected).unwrap();
        fs::create_dir(&selected).unwrap();
        assert_eq!(
            verify_configured_native_catalog(&root, &instance, &settings(&executable))
                .await
                .unwrap_err()
                .code,
            "internal_error",
            "An unreadable selected catalog must not fall back."
        );
        assert_eq!(fs::read(&fallback).unwrap(), fallback_bytes);
    }
}
