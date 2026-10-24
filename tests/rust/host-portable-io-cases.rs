use super::*;

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Directory,
    File(Vec<u8>),
    Link(PathBuf),
}
fn image(root: &Path) -> BTreeMap<PathBuf, Entry> {
    let mut result = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(path) = pending.pop() {
        let info = fs::symlink_metadata(&path).unwrap();
        let value = if info.file_type().is_symlink() {
            Entry::Link(fs::read_link(&path).unwrap())
        } else if info.is_file() {
            Entry::File(fs::read(&path).unwrap())
        } else {
            pending.extend(fs::read_dir(&path).unwrap().map(|v| v.unwrap().path()));
            Entry::Directory
        };
        result.insert(path.strip_prefix(root).unwrap().to_owned(), value);
    }
    result
}
fn roots(fixture: &Fixture, variant: &str) -> (PathBuf, PathBuf) {
    let mut base = fixture.0.join(if variant == "unicode-dot" {
        "目录 space"
    } else {
        "owned"
    });
    #[cfg(windows)]
    if variant.starts_with("opaque") || variant == "trailing" {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};
        let canonical = fs::canonicalize(&fixture.0).unwrap();
        if variant == "trailing" {
            base = canonical.join("owned. ");
            fs::create_dir(fixture.0.join("owned")).unwrap();
            fixture.write("owned/retained", b"unrelated trailing-name alias");
        } else {
            let mut name: Vec<u16> = "owned".encode_utf16().collect();
            name.push(if variant == "opaque-high" {
                0xd800
            } else {
                0xdfff
            });
            base = canonical.join(OsString::from_wide(&name));
            fixture.write("owned�/retained", b"unrelated UTF16 alias");
        }
    }
    fs::create_dir(&base).unwrap();
    let configured = match variant {
        "unicode-dot" => base.join("."),
        "canonical" => fs::canonicalize(&base).unwrap(),
        "linked" => {
            let alias = fixture.0.join("linked");
            ivy_host_runtime::filesystem::directory_link(Path::new("owned"), &alias).unwrap();
            alias
        }
        _ => base,
    };
    let artifact = configured.join("artifact");
    let temporary = configured.join("temporary");
    fs::create_dir(&artifact).unwrap();
    fs::create_dir(&temporary).unwrap();
    fixture.write("untouched", b"unrelated fixture bytes");
    (artifact, temporary)
}
fn package_entries(name: &str) -> Vec<(&'static str, Vec<u8>, u32)> {
    vec![
        (
            "package/package.json",
            json::stringify(
                &IvyJson::object([("name", name.into()), ("version", "1.2.3".into())]),
                usize::MAX,
            )
            .unwrap()
            .into_bytes(),
            0o644,
        ),
        (
            "package/bin/tool",
            b"native payload bytes\0\xff".to_vec(),
            0o755,
        ),
    ]
}
fn archive(entries: &[(&str, Vec<u8>, u32)]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(encoder);
    for (path, bytes, mode) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(*mode);
        header.set_mtime(0);
        header.set_cksum();
        tar.append_data(&mut header, path, bytes.as_slice())
            .unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}
fn installed_image(location: &str, entries: &[(&str, Vec<u8>, u32)]) -> BTreeMap<PathBuf, Entry> {
    let mut result = BTreeMap::from([(PathBuf::new(), Entry::Directory)]);
    let mut directory = PathBuf::new();
    for part in Path::new(location).components() {
        directory.push(part);
        result.insert(directory.clone(), Entry::Directory);
    }
    for (name, bytes, _) in entries {
        let member = name.strip_prefix("package/").unwrap();
        let file = Path::new(location).join(member);
        let mut parent = file.parent();
        while let Some(path) = parent {
            result.insert(path.to_owned(), Entry::Directory);
            parent = path.parent();
        }
        result.insert(file, Entry::File(bytes.clone()));
    }
    result
}
async fn install(
    location: &str,
    bytes: Vec<u8>,
    artifact: PathBuf,
    temporary: PathBuf,
) -> IvyResult<()> {
    portable::install_portable_dependency(
        dependency(location, &bytes),
        bytes,
        artifact,
        temporary,
        BTreeMap::from([("tar".into(), tar())]),
    )
    .await
}
fn archive_path(temporary: &Path, location: &str) -> PathBuf {
    temporary.join(format!(
        "{}.tgz",
        &ivy_contracts::hash::digest(location.as_bytes())[7..]
    ))
}

fn native_tar_install(
    location: &str,
    bytes: &[u8],
    artifact: &Path,
    temporary: &Path,
) -> std::process::ExitStatus {
    let archive: PathBuf = archive_path(temporary, location).components().collect();
    fs::write(&archive, bytes).unwrap();
    let listing = Command::new(tar())
        .arg("-tzf")
        .arg(&archive)
        .current_dir(temporary)
        .output()
        .unwrap();
    let status = if listing.status.success() {
        let destination: PathBuf = artifact.join(location).components().collect();
        fs::create_dir_all(&destination).unwrap();
        Command::new(tar())
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(destination)
            .args(["--strip-components", "1"])
            .current_dir(temporary)
            .output()
            .unwrap()
            .status
    } else {
        listing.status
    };
    fs::remove_file(archive).unwrap();
    status
}

#[tokio::test]
async fn portable_plan_retains_order_supported_cross_host_variants_and_existing_path_shortcuts() {
    let mut variants = vec!["plain", "unicode-dot", "canonical", "linked"];
    #[cfg(windows)]
    variants.extend(["opaque-high", "opaque-low", "trailing"]);
    for variant in variants {
        let fixture = Fixture::new();
        let (artifact, _) = roots(&fixture, variant);
        fs::create_dir(artifact.join("node_modules")).unwrap();
        fs::write(
            artifact.join("node_modules/existing"),
            b"not a package directory",
        )
        .unwrap();
        fs::create_dir(artifact.join("node_modules/directory")).unwrap();
        let mut packages = Vec::new();
        for (name, os, cpu) in [
            ("linux-arm", "linux", "arm64"),
            ("windows-x64", "win32", "x64"),
            ("linux-x64", "linux", "x64"),
            ("windows-arm", "win32", "arm64"),
            ("darwin", "darwin", "arm64"),
            ("wrong-cpu", "linux", "mips"),
        ] {
            let mut item = metadata();
            item.insert("os", IvyJson::Array(vec![os.into()]));
            item.insert("cpu", IvyJson::Array(vec![cpu.into()]));
            packages.push((format!("node_modules/{name}"), item));
        }
        for (name, item) in [
            (
                "unconstrained",
                IvyJson::object(Vec::<(String, IvyJson)>::new()),
            ),
            ("null-os", IvyJson::object([("os", IvyJson::Null)])),
            ("string-cpu", IvyJson::object([("cpu", "x64".into())])),
            ("existing", IvyJson::Null),
            ("directory", IvyJson::Null),
        ] {
            packages.push((format!("node_modules/{name}"), item));
        }
        let selected = [
            "existing",
            "directory",
            "windows-arm",
            "linux-arm",
            "unconstrained",
            "darwin",
            "linux-x64",
            "wrong-cpu",
            "windows-x64",
            "null-os",
            "string-cpu",
            "absent",
            "windows-arm",
        ]
        .map(|name| format!("node_modules/{name}").into())
        .to_vec();
        let before = image(&fixture.0);
        let actual = portable::portable_dependency_plan(
            IvyJson::object([("packages", IvyJson::object(packages))]),
            selected,
            artifact,
        )
        .await
        .unwrap();
        let expected: Vec<_> = ["windows-arm", "linux-arm", "linux-x64", "windows-x64"]
            .map(|name| dependency(&format!("node_modules/{name}"), b"bytes"))
            .into();
        assert_eq!(actual, expected, "{variant}");
        assert_eq!(image(&fixture.0), before, "{variant}");
    }
}

#[tokio::test]
async fn portable_plan_keeps_empty_set_and_pinned_metadata_refusal_order() {
    let fixture = Fixture::new();
    assert!(
        portable::portable_dependency_plan(IvyJson::Null, vec![], fixture.0.clone())
            .await
            .unwrap()
            .is_empty()
    );
    for (lock, expected) in [
        (IvyJson::Null, "internal_error"),
        (
            IvyJson::object(Vec::<(String, IvyJson)>::new()),
            "invalid_arguments",
        ),
        (
            IvyJson::object([("packages", IvyJson::Null)]),
            "invalid_arguments",
        ),
    ] {
        assert_eq!(
            portable::portable_dependency_plan(
                lock,
                vec!["node_modules/fixture".into()],
                fixture.0.clone(),
            )
            .await
            .unwrap_err()
            .code,
            expected
        );
    }
    assert!(
        portable::portable_dependency_plan(
            IvyJson::object([("packages", IvyJson::Array(vec![]))]),
            vec!["node_modules/fixture".into()],
            fixture.0.clone(),
        )
        .await
        .unwrap()
        .is_empty()
    );
    for (field, entry) in [
        ("resolved", "http://registry.invalid/pinned".into()),
        ("resolved", "HTTPS://registry.invalid/pinned".into()),
        ("resolved", IvyJson::Null),
        ("integrity", "sha512-".into()),
        ("integrity", "sha512-AAAA===".into()),
        ("integrity", "sha512-A_A".into()),
        ("integrity", "sha512-AAA\n".into()),
        ("version", IvyJson::Number(123.0)),
    ] {
        let mut item = metadata();
        item.insert(field, entry);
        let lock = IvyJson::object([(
            "packages",
            IvyJson::object([("node_modules/fixture", item)]),
        )]);
        assert_eq!(
            portable::portable_dependency_plan(
                lock,
                vec!["node_modules/fixture".into()],
                fixture.0.clone()
            )
            .await
            .unwrap_err()
            .code,
            "artifact_missing"
        );
    }
    fixture.write("node_modules/parent", b"file");
    let lock = IvyJson::object([(
        "packages",
        IvyJson::object([("node_modules/parent/child", metadata())]),
    )]);
    let result = portable::portable_dependency_plan(
        lock,
        vec!["node_modules/parent/child".into()],
        fixture.0.clone(),
    )
    .await;
    #[cfg(windows)]
    assert_eq!(
        result.unwrap(),
        vec![dependency("node_modules/parent/child", b"bytes")]
    );
    #[cfg(not(windows))]
    assert_eq!(result.unwrap_err().code, "internal_error");
    let lock = IvyJson::object([(
        "packages",
        IvyJson::object([("node_modules/fixture", metadata())]),
    )]);
    let before = image(&fixture.0);
    assert_eq!(
        portable::portable_dependency_plan(
            lock,
            vec!["node_modules/fixture".into()],
            fixture.0.join("invalid\0root")
        )
        .await
        .unwrap_err()
        .code,
        "internal_error"
    );
    assert_eq!(image(&fixture.0), before);
}

#[tokio::test]
async fn system_tar_materializes_exact_normal_scoped_and_nested_package_identities() {
    for (location, name, variant) in [
        ("node_modules/fixture", "fixture", "plain"),
        ("node_modules/@scope/fixture", "@scope/fixture", "plain"),
        (
            "node_modules/outer/node_modules/@scope/fixture",
            "@scope/fixture",
            "plain",
        ),
        ("node_modules/fixture", "fixture", "unicode-dot"),
        ("node_modules/fixture", "fixture", "canonical"),
        ("node_modules/fixture", "fixture", "linked"),
    ] {
        let fixture = Fixture::new();
        let (artifact, temporary) = roots(&fixture, variant);
        let entries = package_entries(name);
        let bytes = archive(&entries);
        let reference = Fixture::new();
        let (reference_artifact, reference_temporary) = roots(&reference, variant);
        let status =
            native_tar_install(location, &bytes, &reference_artifact, &reference_temporary);
        let mut expected = image(&fixture.0);
        let canonical_artifact = fs::canonicalize(&artifact).unwrap();
        let canonical_root = fs::canonicalize(&fixture.0).unwrap();
        let prefix = canonical_artifact.strip_prefix(&canonical_root).unwrap();
        for (path, entry) in image(&reference_artifact) {
            expected.insert(prefix.join(path), entry);
        }
        let result = install(location, bytes, artifact.clone(), temporary.clone()).await;
        if status.success() {
            result.unwrap_or_else(|error| panic!("{location}/{variant}: {error:?}"));
            assert_eq!(image(&artifact), installed_image(location, &entries));
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.code, "command_failed", "{location}/{variant}");
            assert_eq!(
                error.details.as_ref().unwrap().get("exitCode"),
                Some(&IvyJson::Number(status.code().unwrap() as f64)),
                "{location}/{variant}"
            );
        }
        assert_eq!(image(&fixture.0), expected, "{location}/{variant}");
        assert_eq!(
            image(&temporary),
            BTreeMap::from([(PathBuf::new(), Entry::Directory)])
        );
        #[cfg(unix)]
        if status.success() {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(artifact.join(location).join("bin/tool"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
        }
    }
}

#[tokio::test]
async fn system_tar_refusals_cleanup_owned_archives_and_preserve_exclusive_create_collisions() {
    for (label, entries, expected, extracted) in [
        ("empty", vec![], "artifact_changed", false),
        (
            "wrong-root",
            vec![("other/file", b"bytes".to_vec(), 0o644)],
            "artifact_changed",
            false,
        ),
        (
            "missing-json",
            vec![("package/payload", b"bytes".to_vec(), 0o644)],
            "internal_error",
            true,
        ),
        (
            "null-json",
            vec![("package/package.json", b"null".to_vec(), 0o644)],
            "internal_error",
            true,
        ),
        (
            "syntax-json",
            vec![("package/package.json", b"{".to_vec(), 0o644)],
            "internal_error",
            true,
        ),
        (
            "wrong-name",
            vec![(
                "package/package.json",
                br#"{"name":"other","version":"1.2.3"}"#.to_vec(),
                0o644,
            )],
            "build_mismatch",
            true,
        ),
        (
            "wrong-version",
            vec![(
                "package/package.json",
                br#"{"name":"fixture","version":123}"#.to_vec(),
                0o644,
            )],
            "build_mismatch",
            true,
        ),
        (
            "large-json",
            vec![("package/package.json", vec![32; 1024 * 1024 + 1], 0o644)],
            "invalid_arguments",
            true,
        ),
    ] {
        let fixture = Fixture::new();
        let (artifact, temporary) = roots(&fixture, "plain");
        let mut expected_tree = image(&fixture.0);
        if extracted {
            for (path, entry) in installed_image("node_modules/fixture", &entries) {
                expected_tree.insert(Path::new("owned/artifact").join(path), entry);
            }
        }
        let result = install(
            "node_modules/fixture",
            archive(&entries),
            artifact,
            temporary.clone(),
        )
        .await
        .unwrap_err();
        assert_eq!(result.code, expected, "{label}");
        assert_eq!(image(&fixture.0), expected_tree, "{label}");
        assert_eq!(
            image(&temporary),
            BTreeMap::from([(PathBuf::new(), Entry::Directory)])
        );
    }
    let fixture = Fixture::new();
    let (artifact, temporary) = roots(&fixture, "plain");
    let before = image(&fixture.0);
    assert_eq!(
        install(
            "node_modules/fixture",
            b"not a gzip archive".to_vec(),
            artifact,
            temporary
        )
        .await
        .unwrap_err()
        .code,
        "command_failed"
    );
    assert_eq!(image(&fixture.0), before);

    let mut variants = vec!["plain", "canonical", "linked"];
    #[cfg(windows)]
    variants.extend(["opaque-high", "opaque-low", "trailing"]);
    for variant in variants {
        let fixture = Fixture::new();
        let (artifact, temporary) = roots(&fixture, variant);
        let bytes = archive(&package_entries("fixture"));
        let owned = archive_path(&temporary, "node_modules/fixture");
        fs::write(&owned, b"existing writer archive").unwrap();
        let hardlink = temporary.join("writer-hardlink");
        fs::hard_link(&owned, &hardlink).unwrap();
        let before = image(&fixture.0);
        assert_eq!(
            install("node_modules/fixture", bytes, artifact, temporary)
                .await
                .unwrap_err()
                .code,
            "internal_error",
            "{variant}"
        );
        assert_eq!(image(&fixture.0), before, "{variant}");
        assert_eq!(fs::read(hardlink).unwrap(), b"existing writer archive");
    }
    for case in [
        "archive-directory",
        "destination-file",
        "missing-temporary",
        "checksum-first",
    ] {
        let fixture = Fixture::new();
        let (artifact, temporary) = roots(&fixture, "plain");
        let bytes = archive(&package_entries("fixture"));
        let owned = archive_path(&temporary, "node_modules/fixture");
        let mut passed_temporary = temporary.clone();
        if case == "archive-directory" {
            fs::create_dir(&owned).unwrap();
            fs::write(owned.join("retained"), b"another owner").unwrap();
        } else if case == "destination-file" {
            fs::create_dir(artifact.join("node_modules")).unwrap();
            fs::write(
                artifact.join("node_modules/fixture"),
                b"retained destination",
            )
            .unwrap();
        } else if case == "missing-temporary" {
            passed_temporary = temporary.join("missing");
        } else {
            fs::write(&owned, b"retained before checksum refusal").unwrap();
        }
        let before = image(&fixture.0);
        let mut dep = dependency("node_modules/fixture", &bytes);
        if case == "checksum-first" {
            dep.integrity = checksum(b"different bytes");
        }
        let result = portable::install_portable_dependency(
            dep,
            bytes,
            artifact,
            passed_temporary,
            BTreeMap::from([("tar".into(), tar())]),
        )
        .await
        .unwrap_err();
        assert_eq!(
            result.code,
            if case == "checksum-first" {
                "artifact_changed"
            } else {
                "internal_error"
            },
            "{case}"
        );
        assert_eq!(image(&fixture.0), before, "{case}");
    }
}
