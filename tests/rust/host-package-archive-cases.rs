use super::{ServiceFixture, assert_json_eq};
use ivy_host_runtime::{IvyJson, artifact, config, native_path, package_archive};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
struct ReadonlyCleanup(PathBuf);
impl Drop for ReadonlyCleanup {
    fn drop(&mut self) {
        fn clear(path: &Path) {
            let Ok(metadata) = fs::symlink_metadata(path) else {
                return;
            };
            if metadata.file_type().is_symlink() {
                return;
            }
            if metadata.is_dir() {
                for entry in fs::read_dir(path).unwrap().flatten() {
                    clear(&entry.path());
                }
            } else if metadata.permissions().readonly() {
                let mut permissions = metadata.permissions();
                #[cfg(windows)]
                permissions.set_readonly(false);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    permissions.set_mode(permissions.mode() | 0o200);
                }
                let _ = fs::set_permissions(path, permissions);
            }
        }
        assert!(self.0.starts_with(std::env::temp_dir()));
        clear(&self.0);
    }
}
fn path(path: &Path) -> IvyJson {
    IvyJson::String(native_path::from_native(path))
}
async fn fixture(component: &str, app: bool) -> (ServiceFixture, IvyJson, IvyJson) {
    let (fixture, mut candidate, mut manifest) = super::artifact_cases::fixture().await;
    let package = fixture.0.join("package");
    let root = package.join("artifact");
    fs::create_dir_all(root.join("dist/apps/fixture")).unwrap();
    manifest.insert("componentId", component.into());
    candidate.insert("componentId", component.into());
    candidate.insert("artifactRoot", path(&root));
    candidate.insert("manifestPath", path(&package.join("component.json")));
    if !app {
        manifest.insert("kind", "service".into());
        manifest.remove("app");
        let command = |name: &str| {
            IvyJson::object([
                ("executable", "node".into()),
                ("args", IvyJson::Array(vec![name.into()])),
                ("timeoutMs", IvyJson::Number(10000.0)),
            ])
        };
        manifest.insert("entrypoint", command("dist/main.js"));
        manifest.insert(
            "readiness",
            IvyJson::object([
                ("timeoutMs", IvyJson::Number(10000.0)),
                ("command", command("dist/health.js")),
            ]),
        );
        manifest.insert(
            "shutdown",
            IvyJson::object([("timeoutMs", IvyJson::Number(5000.0))]),
        );
        manifest.insert(
            "restart",
            IvyJson::object([
                ("policy", "never".into()),
                ("minimumDelayMs", IvyJson::Number(100.0)),
                ("maximumDelayMs", IvyJson::Number(1000.0)),
            ]),
        );
    }
    fs::write(
        root.join("dist/main.js"),
        b"throw Error('archive validation must not execute JS');",
    )
    .unwrap();
    fs::write(
        root.join("dist/health.js"),
        b"throw Error('archive validation must not execute health');",
    )
    .unwrap();
    fs::write(root.join("dist/apps/fixture/index.html"), b"<p>fixture</p>").unwrap();
    fs::write(
        root.join("dist/read me #=data.txt"),
        b"\0\xff\xfe\n preserved bytes",
    )
    .unwrap();
    fs::write(
        root.join("dist")
            .join(format!("{}-data.txt", "long-".repeat(22))),
        b"long name",
    )
    .unwrap();
    config::atomic_json(package.join("component.json"), manifest.clone())
        .await
        .unwrap();
    config::atomic_json(
        root.join("dist/build-info.json"),
        IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("componentId", component.into()),
            ("version", manifest.required("version").unwrap().clone()),
            ("buildId", candidate.required("buildId").unwrap().clone()),
        ]),
    )
    .await
    .unwrap();
    (fixture, candidate, manifest)
}
fn authorization(manifest: &IvyJson) -> IvyJson {
    IvyJson::object([
        (
            "componentId",
            manifest.required("componentId").unwrap().clone(),
        ),
        ("version", manifest.required("version").unwrap().clone()),
        ("buildId", manifest.required("buildId").unwrap().clone()),
        ("archiveHash", format!("sha256:{}", "b".repeat(64)).into()),
        ("bytes", IvyJson::Number(1.0)),
        ("manifest", manifest.clone()),
        ("extra", "ignored".into()),
    ])
}

#[tokio::test]
async fn package_authorization_and_catalog_validate_public_identities_and_limits() {
    let (_fixture, _, manifest) = fixture("fixture", true).await;
    let good = authorization(&manifest);
    let mut projected = good.clone();
    projected.remove("extra");
    assert_json_eq(
        &package_archive::validate_package_authorization(&good).unwrap(),
        &projected,
    );
    let mut maximum = good.clone();
    maximum.insert(
        "bytes",
        IvyJson::Number(package_archive::MAXIMUM_ARCHIVE_BYTES as f64),
    );
    package_archive::validate_package_authorization(&maximum).unwrap();
    let mut invalid = vec![
        IvyJson::Null,
        IvyJson::Array(vec![]),
        true.into(),
        IvyJson::Number(1.0),
        "authorization".into(),
        IvyJson::object([] as [(&str, IvyJson); 0]),
    ];
    for value in [
        None,
        Some(IvyJson::Null),
        Some(IvyJson::Array(vec![])),
        Some(IvyJson::Number(1.0)),
        Some("manifest".into()),
        Some(IvyJson::object([] as [(&str, IvyJson); 0])),
    ] {
        let mut entry = good.clone();
        if let Some(value) = value {
            entry.insert("manifest", value);
        } else {
            entry.remove("manifest");
        }
        invalid.push(entry);
    }
    for (key, value) in [
        ("componentId", "bad_id".into()),
        ("componentId", "different".into()),
        ("version", "01.0.0".into()),
        (
            "version",
            IvyJson::String(ivy_contracts::JsString(vec![0xd800])),
        ),
        ("buildId", "sha256:bad".into()),
        ("archiveHash", format!("sha256:{}", "A".repeat(64)).into()),
        ("bytes", IvyJson::Number(0.0)),
        ("bytes", IvyJson::Number(1.5)),
        (
            "bytes",
            IvyJson::Number(package_archive::MAXIMUM_ARCHIVE_BYTES as f64 + 1.0),
        ),
        ("bytes", "1".into()),
    ] {
        let mut entry = good.clone();
        entry.insert(key, value);
        invalid.push(entry);
    }
    for value in invalid {
        assert!(
            package_archive::validate_package_authorization(&value).is_err(),
            "{value:?}"
        );
    }
    let index = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("revision", IvyJson::Number(0.0)),
        (
            "packages",
            IvyJson::Array(vec![IvyJson::object([
                ("componentId", "fixture".into()),
                ("version", "999999999999999999999999.0.0".into()),
                (
                    "manifest",
                    IvyJson::object([("schemaVersion", IvyJson::Number(999.0))]),
                ),
            ])]),
        ),
        ("extension", true.into()),
    ]);
    assert_json_eq(
        &package_archive::validate_package_catalog_index(&index).unwrap(),
        &index,
    );
    assert!(package_archive::validate_package_catalog_index(&IvyJson::Null).is_err());
    for (key, value) in [
        ("schemaVersion", IvyJson::Number(2.0)),
        ("revision", IvyJson::Number(-1.0)),
        ("revision", IvyJson::Number(9_007_199_254_740_992.0)),
        ("packages", IvyJson::Array(vec![IvyJson::Null])),
        (
            "packages",
            IvyJson::Array(vec![IvyJson::object([
                ("componentId", "fixture".into()),
                ("version", "1.0.0-beta".into()),
            ])]),
        ),
        (
            "packages",
            IvyJson::Array(vec![
                IvyJson::object([
                    ("componentId", "fixture".into()),
                    ("version", "1.0.0".into())
                ]);
                4097
            ]),
        ),
    ] {
        let mut value_case = index.clone();
        value_case.insert(key, value);
        assert!(package_archive::validate_package_catalog_index(&value_case).is_err());
    }
    let mut entry = good;
    entry.insert("revision", IvyJson::Number(1.0));
    entry.insert("publishedAt", "2026-10-18T00:00:00.000Z".into());
    entry.insert("publisherPrincipalId", "publisher".into());
    let catalog = |entry| {
        IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("revision", IvyJson::Number(1.0)),
            ("packages", IvyJson::Array(vec![entry])),
        ])
    };
    let valid = catalog(entry.clone());
    assert_json_eq(
        &package_archive::validate_package_catalog(&valid).unwrap(),
        &valid,
    );
    let mut offset = entry.clone();
    offset.insert("publishedAt", "2026-10-18T12:34:56+02:00".into());
    package_archive::validate_package_catalog(&catalog(offset)).unwrap();
    for (key, value) in [
        ("revision", IvyJson::Number(0.0)),
        ("revision", IvyJson::Number(1.5)),
        ("publishedAt", "invalid".into()),
        ("publishedAt", IvyJson::Number(0.0)),
        ("publisherPrincipalId", "".into()),
        ("publisherPrincipalId", "a".repeat(257).into()),
    ] {
        let mut value_case = entry.clone();
        value_case.insert(key, value);
        assert!(package_archive::validate_package_catalog(&catalog(value_case)).is_err());
    }
    let mut maximum_publisher = entry.clone();
    maximum_publisher.insert("publisherPrincipalId", "a".repeat(256).into());
    package_archive::validate_package_catalog(&catalog(maximum_publisher)).unwrap();
    for value in [None, Some(IvyJson::Null)] {
        let mut value_case = entry.clone();
        if let Some(value) = value {
            value_case.insert("manifest", value);
        } else {
            value_case.remove("manifest");
        }
        assert!(package_archive::validate_package_catalog(&catalog(value_case)).is_err());
    }
}

#[tokio::test]
async fn ordinary_archive_round_trips_preserve_files_modes_and_manifests() {
    for (app, custom) in [(true, false), (false, false), (true, true)] {
        let (fixture, candidate, mut manifest) = fixture("fixture", app).await;
        let root = native_root(&candidate);
        if !custom {
            fs::create_dir(root.join("dist/ü 空 😀")).unwrap();
            fs::write(
                root.join("dist/ü 空 😀/read me #=ü 空 😀.txt"),
                b"\0\xff exact Unicode payload",
            )
            .unwrap();
        }
        let executables = if custom {
            let tools = platform_archive_tools();
            let tar = fixture.0.join(if cfg!(windows) {
                "configured-tar.exe"
            } else {
                "configured-tar"
            });
            fs::copy(tools.get("tar").unwrap(), &tar).unwrap();
            BTreeMap::from([("tar".into(), tar)])
        } else if app {
            platform_archive_tools()
        } else {
            BTreeMap::new()
        };
        if !app {
            for key in ["entrypoint", "readiness"] {
                let mut command = if key == "entrypoint" {
                    manifest.required(key).unwrap().clone()
                } else {
                    manifest
                        .required(key)
                        .unwrap()
                        .required("command")
                        .unwrap()
                        .clone()
                };
                command.insert(
                    "args",
                    IvyJson::Array(vec![
                        format!(
                            "dist/not-created/../{}",
                            if key == "entrypoint" {
                                "main.js"
                            } else {
                                "health.js"
                            }
                        )
                        .into(),
                    ]),
                );
                if key == "entrypoint" {
                    manifest.insert(key, command);
                } else {
                    let mut readiness = manifest.required(key).unwrap().clone();
                    readiness.insert("command", command);
                    manifest.insert(key, readiness);
                }
            }
            config::atomic_json(
                root.parent().unwrap().join("component.json"),
                manifest.clone(),
            )
            .await
            .unwrap();
        }
        let before = native_tree(&root);
        let archive =
            package_archive::create_package_archive(candidate.clone(), executables.clone())
                .await
                .unwrap();
        assert_eq!(archive.bytes, fs::metadata(&archive.path).unwrap().len());
        assert_eq!(
            archive.hash,
            artifact::file_hash(archive.path.clone()).await.unwrap()
        );
        let saved = fixture.0.join("received.tar.gz");
        fs::copy(&archive.path, &saved).unwrap();
        let extraction = fixture.0.join("extracted");
        fs::create_dir(&extraction).unwrap();
        fs::write(extraction.join("stale"), b"discard after admission").unwrap();
        package_archive::validate_package_archive(
            saved.clone(),
            archive.hash.clone(),
            manifest.clone(),
            extraction.clone(),
            executables,
        )
        .await
        .unwrap();
        assert!(!extraction.join("stale").exists());
        assert_eq!(native_tree(&extraction.join("artifact")), before);
        assert_json_eq(
            &config::json_file(extraction.join("component.json"), 65536)
                .await
                .unwrap(),
            &manifest,
        );
        // An explicitly different tool remains authoritative, including errors.
        let missing = BTreeMap::from([("tar".into(), fixture.0.join("missing-tar"))]);
        assert!(
            package_archive::create_package_archive(candidate, missing.clone())
                .await
                .is_err()
        );
        assert_eq!(
            archive.hash,
            artifact::file_hash(archive.path).await.unwrap()
        );
        assert!(
            package_archive::validate_package_archive(
                saved,
                archive.hash,
                manifest,
                extraction.clone(),
                missing,
            )
            .await
            .is_err()
        );
    }
}

fn platform_archive_tools() -> BTreeMap<String, PathBuf> {
    let tar = if cfg!(windows) {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/tar.exe")
    } else {
        PathBuf::from("/usr/bin/tar")
    };
    BTreeMap::from([("tar".into(), tar)])
}

fn append_fixture_entry<W: Write>(
    archive: &mut tar::Builder<W>,
    name: &str,
    bytes: &[u8],
    link: Option<&str>,
) {
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(bytes.len() as u64);
    if let Some(link) = link {
        header.set_entry_type(tar::EntryType::Symlink);
        assert!(link.len() < 100);
        header.as_mut_bytes()[157..157 + link.len()].copy_from_slice(link.as_bytes());
    }
    if name.len() < 100 {
        // Deliberately encode unsafe names to exercise the receiver's admission.
        header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
        header.set_cksum();
        archive.append(&header, bytes).unwrap();
    } else {
        assert!(link.is_none());
        archive.append_data(&mut header, name, bytes).unwrap();
    }
}

fn mutate_archive(archive: &Path, case: &str, package: &Path) {
    let entries = {
        let mut input = tar::Archive::new(flate2::read::GzDecoder::new(
            fs::File::open(archive).unwrap(),
        ));
        input
            .entries()
            .unwrap()
            .map(|entry| {
                let mut entry = entry.unwrap();
                let name = entry.path().unwrap().into_owned();
                let header = entry.header().clone();
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                (name, header, bytes)
            })
            .collect::<Vec<_>>()
    };
    let output = flate2::write::GzEncoder::new(
        fs::File::create(archive).unwrap(),
        flate2::Compression::default(),
    );
    let mut output = tar::Builder::new(output);
    for (name, mut header, bytes) in entries {
        let link = match case {
            "component-link" if name == Path::new("component.json") => {
                Some("artifact/manifest.json")
            }
            "artifact-link" | "custom-artifact-link" if name == Path::new("artifact") => {
                Some("../outside")
            }
            "payload-link" if name == Path::new("artifact/dist/main.js") => Some("health.js"),
            _ => None,
        };
        if let Some(link) = link {
            append_fixture_entry(&mut output, name.to_str().unwrap(), &[], Some(link));
        } else {
            output
                .append_data(&mut header, &name, bytes.as_slice())
                .unwrap();
        }
    }
    match case {
        "unexpected-root" => append_fixture_entry(&mut output, "extra", b"unexpected", None),
        "parent-path" => {
            append_fixture_entry(&mut output, "../escape", b"contained fixture only", None)
        }
        "backslash" => {
            append_fixture_entry(&mut output, "artifact\\escape", b"unsupported path", None)
        }
        "ads" => append_fixture_entry(
            &mut output,
            "artifact/dist/main.js:alternate",
            b"unsupported stream",
            None,
        ),
        "reserved-name" => append_fixture_entry(
            &mut output,
            "artifact/dist/CON.txt",
            b"unsupported alias",
            None,
        ),
        "path-alias" => append_fixture_entry(
            &mut output,
            "artifact/dist/main.js.",
            b"unsupported alias",
            None,
        ),
        "duplicate" => append_fixture_entry(
            &mut output,
            "artifact/dist/main.js",
            &fs::read(package.join("artifact/dist/main.js")).unwrap(),
            None,
        ),
        "entry-limit" => {
            for i in 0..50001 {
                append_fixture_entry(&mut output, &format!("artifact/file-{i}"), &[], None);
            }
        }
        "listing-limit" => {
            for i in 0..6000 {
                append_fixture_entry(
                    &mut output,
                    &format!("artifact/{}{i}", "a".repeat(1790)),
                    &[],
                    None,
                );
            }
        }
        "metadata-limit" => {
            output
                .append_pax_extensions([("comment", vec![b'a'; 8 * 1024 * 1024 + 8192].as_slice())])
                .unwrap();
            append_fixture_entry(&mut output, "artifact/metadata", b"bounded metadata", None);
        }
        _ => (),
    }
    output.into_inner().unwrap().finish().unwrap();
}

#[tokio::test]
async fn archive_receive_refuses_invalid_packages_and_preserves_outside_files() {
    for mode in [
        "hash",
        "manifest",
        "stamp-build",
        "stamp-version",
        "stamp-schema",
        "empty-native",
        "missing-native",
        "directory-native",
        "missing-app-entry",
        "component-link",
        "artifact-link",
        "custom-artifact-link",
        "unexpected-root",
        "parent-path",
        "backslash",
        "ads",
        "reserved-name",
        "path-alias",
        "duplicate",
        "payload-link",
        "entry-limit",
        "listing-limit",
        "metadata-limit",
    ] {
        // These legacy runtime helpers are required only by Windows receivers.
        if mode.ends_with("native") && !cfg!(windows) {
            continue;
        }
        if ["ads", "reserved-name", "path-alias"].contains(&mode) && !cfg!(windows) {
            continue;
        }
        let (fixture, candidate, mut manifest) = fixture(
            if mode.ends_with("native") {
                "agent-manager"
            } else {
                "fixture"
            },
            true,
        )
        .await;
        let root = native_root(&candidate);
        let package = root.parent().unwrap();
        if mode.ends_with("native") {
            let mut app = manifest.required("app").unwrap().clone();
            app.insert("appId", manifest.required("componentId").unwrap().clone());
            manifest.insert("app", app);
            config::atomic_json(package.join("component.json"), manifest.clone())
                .await
                .unwrap();
            fs::create_dir_all(root.join("dist/native")).unwrap();
            if mode == "directory-native" {
                fs::create_dir(root.join("dist/native/ivy-job.exe")).unwrap();
            } else if mode == "empty-native" {
                fs::write(root.join("dist/native/ivy-job.exe"), b"").unwrap();
            }
        }
        if mode.starts_with("stamp-") {
            let mut stamp = config::json_file(root.join("dist/build-info.json"), 1024)
                .await
                .unwrap();
            match mode {
                "stamp-build" => {
                    stamp.insert("buildId", format!("sha256:{}", "c".repeat(64)).into())
                }
                "stamp-version" => stamp.insert("version", "2.0.0".into()),
                _ => stamp.insert("schemaVersion", IvyJson::Number(2.0)),
            };
            config::atomic_json(root.join("dist/build-info.json"), stamp)
                .await
                .unwrap();
        }
        if mode == "missing-app-entry" {
            fs::remove_file(root.join("dist/apps/fixture/index.html")).unwrap();
        }
        fs::create_dir(fixture.0.join("outside")).unwrap();
        fs::write(
            fixture.0.join("outside/retained"),
            b"retained outside bytes",
        )
        .unwrap();
        let outside = native_tree(&fixture.0.join("outside"));
        fs::write(fixture.0.join("escape"), b"retained escape bytes").unwrap();
        let archive = package_archive::create_package_archive(candidate, BTreeMap::new())
            .await
            .unwrap();
        if [
            "component-link",
            "artifact-link",
            "custom-artifact-link",
            "unexpected-root",
            "parent-path",
            "backslash",
            "ads",
            "reserved-name",
            "path-alias",
            "duplicate",
            "payload-link",
            "entry-limit",
            "listing-limit",
            "metadata-limit",
        ]
        .contains(&mode)
        {
            mutate_archive(&archive.path, mode, package);
        }
        let hash = if mode == "hash" {
            format!("sha256:{}", "0".repeat(64))
        } else {
            artifact::file_hash(archive.path.clone()).await.unwrap()
        };
        if mode == "manifest" {
            manifest.insert("version", "2.0.0".into());
        }
        let extraction = fixture.0.join("extract");
        fs::create_dir(&extraction).unwrap();
        fs::write(extraction.join("stale"), b"retained before admission").unwrap();
        let result = package_archive::validate_package_archive(
            archive.path,
            hash,
            manifest,
            extraction.clone(),
            if mode == "custom-artifact-link" {
                BTreeMap::from([("tar".into(), fixture.0.join("missing-custom-extractor"))])
            } else {
                BTreeMap::new()
            },
        )
        .await;
        if mode == "duplicate" {
            result.unwrap();
            assert_eq!(
                native_tree(&root),
                native_tree(&extraction.join("artifact"))
            );
        } else {
            assert!(result.is_err(), "{mode}");
            let expected = match mode {
                "manifest" | "stamp-build" | "stamp-version" | "stamp-schema" => "build_mismatch",
                "missing-native" => "artifact_missing",
                "missing-app-entry" => "internal_error",
                "listing-limit" | "metadata-limit" => "limit_exceeded",
                _ => "artifact_changed",
            };
            assert_eq!(result.as_ref().unwrap_err().code, expected, "{mode}");
            if [
                "hash",
                "missing-native",
                "unexpected-root",
                "parent-path",
                "backslash",
                "ads",
                "reserved-name",
                "path-alias",
                "artifact-link",
                "custom-artifact-link",
                "component-link",
                "payload-link",
                "entry-limit",
                "listing-limit",
                "metadata-limit",
            ]
            .contains(&mode)
            {
                assert_eq!(
                    fs::read(extraction.join("stale")).unwrap(),
                    b"retained before admission",
                    "{mode}"
                );
            } else {
                assert!(!extraction.join("stale").exists(), "{mode}");
            }
        }
        assert_eq!(native_tree(&fixture.0.join("outside")), outside, "{mode}");
        assert_eq!(
            fs::read(fixture.0.join("escape")).unwrap(),
            b"retained escape bytes",
            "{mode}"
        );
    }
}

fn native_root(candidate: &IvyJson) -> PathBuf {
    native_path::to_native(
        candidate
            .required("artifactRoot")
            .unwrap()
            .as_js_string()
            .unwrap(),
    )
}

async fn native_fixture(unicode: bool) -> (ServiceFixture, IvyJson, IvyJson, Vec<PathBuf>) {
    let (fixture, mut candidate, mut manifest) = fixture("fixture", false).await;
    let name = if unicode {
        "native ü 空 😀"
    } else {
        "native package #="
    };
    let package = fixture.0.join(name);
    #[cfg(windows)]
    let package = if unicode {
        package
    } else {
        use std::os::windows::ffi::OsStringExt;
        let mut name = package.into_os_string();
        name.push(std::ffi::OsString::from_wide(&[0xd800]));
        PathBuf::from(name)
    };
    fs::rename(fixture.0.join("package"), &package).unwrap();
    let root = package.join("artifact");
    candidate.insert("artifactRoot", path(&root));
    candidate.insert("manifestPath", path(&package.join("component.json")));
    let target = format!(
        "{}-{}",
        if cfg!(windows) { "win32" } else { "linux" },
        if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "x64"
        }
    );
    let command = |name: &str| {
        IvyJson::object([
            (
                "executable",
                format!(
                    "dist/native/{target}/{name}{}",
                    if cfg!(windows) { ".exe" } else { "" }
                )
                .into(),
            ),
            ("args", IvyJson::Array(Vec::new())),
            ("timeoutMs", IvyJson::Number(10000.0)),
        ])
    };
    manifest.insert("kind", "native".into());
    manifest.insert("entrypoint", command("ivy-fixture"));
    manifest.insert(
        "readiness",
        IvyJson::object([
            ("timeoutMs", IvyJson::Number(10000.0)),
            ("command", command("ivy-health")),
        ]),
    );
    config::atomic_json(package.join("component.json"), manifest.clone())
        .await
        .unwrap();
    let stamp = config::json_file(root.join("dist/build-info.json"), 65536)
        .await
        .unwrap();
    let mut files = Vec::new();
    for platform in ["win32-x64", "win32-arm64", "linux-x64", "linux-arm64"] {
        let directory = root.join("dist/native").join(platform);
        fs::create_dir_all(&directory).unwrap();
        config::atomic_json(directory.join("build-info.json"), stamp.clone())
            .await
            .unwrap();
        for name in ["ivy-fixture", "ivy-health", "ivy-extra"] {
            let file = directory.join(format!(
                "{name}{}",
                if platform.starts_with("win32-") {
                    ".exe"
                } else {
                    ""
                }
            ));
            fs::write(
                &file,
                format!("nonexecutable fixture bytes:{platform}/{name}"),
            )
            .unwrap();
            if platform.starts_with("linux-") {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
                }
                files.push(file);
            }
        }
    }
    fs::write(
        root.join(if unicode {
            "dist/read me #=ü 空 😀.txt"
        } else {
            "dist/read me #=.txt"
        }),
        b"\0\xff\xfe\n preserved bytes",
    )
    .unwrap();
    fs::write(
        root.join("dist").join(format!(
            "{}-{}.txt",
            "long-".repeat(22),
            if unicode { "ü" } else { "name" }
        )),
        b"long filename",
    )
    .unwrap();
    let readonly = root.join("dist/readonly.dat");
    fs::write(&readonly, b"readonly payload").unwrap();
    let mut permissions = fs::metadata(&readonly).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(readonly, permissions).unwrap();
    (fixture, candidate, manifest, files)
}

fn native_tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, u32)> {
    let mut files = BTreeMap::new();
    let mut directories = vec![root.to_owned()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            assert!(!metadata.file_type().is_symlink());
            if metadata.is_dir() {
                directories.push(entry.path());
            } else {
                assert!(metadata.is_file());
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.permissions().mode() & 0o777
                };
                #[cfg(not(unix))]
                let mode = 0;
                files.insert(
                    entry.path().strip_prefix(root).unwrap().to_owned(),
                    (fs::read(entry.path()).unwrap(), mode),
                );
            }
        }
    }
    files
}

fn native_headers(archive: &Path) -> BTreeMap<PathBuf, (tar::EntryType, u32)> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(
        fs::File::open(archive).unwrap(),
    ));
    let mut headers = BTreeMap::new();
    for (index, entry) in archive.entries().unwrap().enumerate() {
        let entry = entry.unwrap();
        let name = entry.path().unwrap().into_owned();
        if index == 0 {
            assert_eq!(name, Path::new("component.json"));
        }
        let header = entry.header();
        assert!(
            headers
                .insert(name, (header.entry_type(), header.mode().unwrap()))
                .is_none(),
            "duplicate archive header"
        );
    }
    headers
}

#[tokio::test]
async fn native_archive_preserves_selected_executable_modes_and_exact_files() {
    let (fixture, candidate, manifest, files) = native_fixture(false).await;
    let _cleanup = ReadonlyCleanup(fixture.0.clone());
    assert_eq!(files.len(), 6);
    let root = native_root(&candidate);
    let before: Vec<_> = files
        .iter()
        .map(|file| fs::metadata(file).unwrap().permissions())
        .collect();
    let selected = files
        .iter()
        .map(|file| {
            file.parent()
                .unwrap()
                .join(".")
                .join(file.file_name().unwrap())
        })
        .collect();
    let archive =
        package_archive::create_package_archive_with_linux_executables(candidate.clone(), selected)
            .await
            .unwrap();
    assert_eq!(archive.bytes, fs::metadata(&archive.path).unwrap().len());
    assert_eq!(
        archive.hash,
        artifact::file_hash(archive.path.clone()).await.unwrap()
    );
    for (file, permissions) in files.iter().zip(before) {
        assert_eq!(fs::metadata(file).unwrap().permissions(), permissions);
    }
    let saved = fixture.0.join("native-selected.tar.gz");
    fs::copy(&archive.path, &saved).unwrap();
    let headers = native_headers(&saved);
    for file in &files {
        let name = Path::new("artifact").join(file.strip_prefix(&root).unwrap());
        let (kind, mode) = headers.get(&name).unwrap();
        assert!(kind.is_file());
        assert_eq!(*mode, 0o755, "{name:?}");
    }
    #[cfg(windows)]
    for (name, (kind, mode)) in &headers {
        if kind.is_file()
            && !name.starts_with("artifact/dist/native/linux-x64")
            && !name.starts_with("artifact/dist/native/linux-arm64")
        {
            assert_eq!(
                *mode,
                if name.ends_with("readonly.dat") {
                    0o444
                } else {
                    0o666
                },
                "{name:?}"
            );
        }
    }
    let received = fixture.0.join("received");
    package_archive::validate_package_archive(
        saved,
        archive.hash,
        manifest.clone(),
        received.clone(),
        BTreeMap::new(),
    )
    .await
    .unwrap();
    assert_eq!(native_tree(&root), native_tree(&received.join("artifact")));
    assert_json_eq(
        &config::json_file(received.join("component.json"), 65536)
            .await
            .unwrap(),
        &manifest,
    );
    let mut candidate = candidate;
    candidate.insert("artifactRoot", path(&received.join("artifact")));
    candidate.insert("manifestPath", path(&received.join("component.json")));
    assert_json_eq(
        &artifact::verify_native_candidate(candidate, received)
            .await
            .unwrap(),
        &manifest,
    );
}

#[tokio::test]
async fn native_unicode_headers_and_receiver_preserve_exact_names_and_bytes() {
    let (fixture, candidate, manifest, files) = native_fixture(true).await;
    let _cleanup = ReadonlyCleanup(fixture.0.clone());
    let root = native_root(&candidate);
    let archive = package_archive::create_package_archive_with_linux_executables(candidate, files)
        .await
        .unwrap();
    let saved = fixture.0.join("unicode.tar.gz");
    fs::copy(&archive.path, &saved).unwrap();
    let headers = native_headers(&saved);
    assert!(headers.contains_key(Path::new("artifact/dist/read me #=ü 空 😀.txt")));
    assert!(
        headers.contains_key(
            &Path::new("artifact/dist").join(format!("{}-ü.txt", "long-".repeat(22)))
        )
    );
    let received = fixture.0.join("unicode-received");
    let result = package_archive::validate_package_archive(
        saved,
        archive.hash,
        manifest,
        received.clone(),
        platform_archive_tools(),
    )
    .await;
    result.unwrap();
    assert_eq!(native_tree(&root), native_tree(&received.join("artifact")));
}

#[tokio::test]
async fn native_archive_rejects_invalid_explicit_modes_and_linked_inputs_before_writing() {
    let (fixture, candidate, _, files) = native_fixture(false).await;
    let _cleanup = ReadonlyCleanup(fixture.0.clone());
    let root = native_root(&candidate);
    let destination = root.parent().unwrap().join("package.tar.gz");
    let outside = fixture.0.join("outside");
    fs::write(&outside, b"retained outside bytes").unwrap();
    fs::write(
        root.join("dist/native/linux-x64/invalid.dll"),
        b"retained bytes",
    )
    .unwrap();
    fs::create_dir_all(root.join("dist/native/linux-x64/nested")).unwrap();
    fs::write(
        root.join("dist/native/linux-x64/nested/ivy-fixture"),
        b"retained bytes",
    )
    .unwrap();
    for (case, selected) in [
        ("outside", vec![outside.clone()]),
        (
            "relative",
            vec![PathBuf::from("dist/native/linux-x64/ivy-fixture")],
        ),
        (
            "windows",
            vec![root.join("dist/native/win32-x64/ivy-fixture.exe")],
        ),
        ("directory", vec![root.join("dist/native/linux-x64")]),
        ("missing", vec![root.join("dist/native/linux-x64/missing")]),
        (
            "duplicate",
            vec![
                files[0].clone(),
                files[0]
                    .parent()
                    .unwrap()
                    .join(".")
                    .join(files[0].file_name().unwrap()),
            ],
        ),
        (
            "extension",
            vec![root.join("dist/native/linux-x64/invalid.dll")],
        ),
        (
            "nested",
            vec![root.join("dist/native/linux-x64/nested/ivy-fixture")],
        ),
    ] {
        let result = package_archive::create_package_archive_with_linux_executables(
            candidate.clone(),
            selected,
        )
        .await;
        assert_eq!(
            result.unwrap_err().code,
            if case == "missing" {
                "internal_error"
            } else {
                "invalid_arguments"
            },
            "{case}"
        );
        assert!(!destination.exists());
        assert_eq!(fs::read(&outside).unwrap(), b"retained outside bytes");
    }
    let selected = root.join("dist/native/linux-x64/empty");
    fs::write(&selected, b"").unwrap();
    assert!(
        package_archive::create_package_archive_with_linux_executables(
            candidate.clone(),
            vec![selected]
        )
        .await
        .is_err()
    );
    assert!(!destination.exists());
    let link = root.join("dist/link");
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&outside, &link).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let result =
        package_archive::create_package_archive_with_linux_executables(candidate, files).await;
    assert!(result.is_err());
    assert!(!destination.exists());
    assert_eq!(fs::read(outside).unwrap(), b"retained outside bytes");
}

#[tokio::test]
async fn native_archive_stops_at_the_existing_compressed_byte_limit() {
    let (fixture, candidate, _, files) = native_fixture(false).await;
    let _cleanup = ReadonlyCleanup(fixture.0.clone());
    let root = native_root(&candidate);
    let mut bytes = vec![0u8; 1024 * 1024];
    let mut state = 0x9e3779b97f4a7c15u64;
    for block in bytes.chunks_exact_mut(8) {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        block.copy_from_slice(&state.wrapping_mul(0x2545f4914f6cdd1d).to_le_bytes());
    }
    let mut file = fs::File::create(root.join("dist/incompressible.bin")).unwrap();
    for _ in 0..=package_archive::MAXIMUM_ARCHIVE_BYTES / bytes.len() as u64 {
        file.write_all(&bytes).unwrap();
    }
    drop(file);
    let error = package_archive::create_package_archive_with_linux_executables(candidate, files)
        .await
        .unwrap_err();
    assert_eq!(error.code, "artifact_changed", "{error:?}");
    assert!(
        fs::metadata(root.parent().unwrap().join("package.tar.gz"))
            .unwrap()
            .len()
            <= package_archive::MAXIMUM_ARCHIVE_BYTES
    );
}
