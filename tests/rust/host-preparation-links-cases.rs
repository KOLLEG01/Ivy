use ivy_host_runtime::{IvyError, IvyResult, concurrency, filesystem, preparation_links};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir()).unwrap();
        let mut name =
            std::ffi::OsString::from(format!("ivy-preparation-links-{}-", uuid::Uuid::new_v4()));
        name.push(opaque_name());
        let root = base.join(name);
        assert_eq!(root.parent(), Some(base.as_path()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn write(&self, local: &str, bytes: &[u8]) -> PathBuf {
        let file = self.0.join(local);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, bytes).unwrap();
        file
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        #[cfg(windows)]
        fn writable(path: &Path) {
            let Ok(metadata) = fs::symlink_metadata(path) else {
                return;
            };
            if metadata.file_type().is_symlink() {
                return;
            }
            if metadata.is_dir() {
                for entry in fs::read_dir(path).unwrap().flatten() {
                    writable(&entry.path());
                }
            }
            if metadata.permissions().readonly() {
                let mut permissions = metadata.permissions();
                permissions.set_readonly(false);
                let _ = fs::set_permissions(path, permissions);
            }
        }
        #[cfg(windows)]
        writable(&self.0);
        let base = fs::canonicalize(std::env::temp_dir()).unwrap();
        if self.0.parent() == Some(base.as_path()) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
fn opaque_name() -> std::ffi::OsString {
    let mut name = std::ffi::OsString::from("native ü 空 😀; ' ");
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        name.push(std::ffi::OsString::from_wide(&[0xd800]));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        name.push(std::ffi::OsString::from_vec(vec![0xff]));
    }
    name
}
fn directory_link(source: &Path, destination: &Path) {
    #[cfg(windows)]
    filesystem::directory_link(source, destination).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(source, destination).unwrap();
}
fn relative_directory_link(target: &str, destination: &Path) {
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(target.replace('/', "\\"), destination).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, destination).unwrap();
}
fn file_link(source: &Path, destination: &Path) {
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(source, destination).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(source, destination).unwrap();
}
fn succeeds(result: &IvyResult<()>) {
    assert!(result.is_ok(), "{result:?}");
}
fn refuses(result: &IvyResult<()>, code: &str) {
    assert_eq!(result.as_ref().unwrap_err().code, code);
}
async fn copy(source: &Path, destination: &Path) -> IvyResult<()> {
    preparation_links::link_dependency_directory(source.to_owned(), destination.to_owned()).await
}

#[tokio::test]
async fn cached_directory_packages_and_scopes_link_while_files_share_identity() {
    let fixture = Fixture::new();
    let source = fixture.0.join("cache ü 空 😀/node_modules");
    fixture.write(
        "cache ü 空 😀/node_modules/pkg/deep/value",
        b"directory bytes",
    );
    fixture.write(
        "cache ü 空 😀/node_modules/@scope/pkg/deep/value",
        b"scoped bytes",
    );
    let scoped = fixture.write(
        "cache ü 空 😀/node_modules/@scope/ordinary",
        b"scoped hardlink",
    );
    let ordinary = fixture.write(
        "cache ü 空 😀/node_modules/.package-lock.json",
        b"shared bytes",
    );
    fs::create_dir_all(source.join("@empty")).unwrap();
    for index in 0..20 {
        fixture.write(
            &format!("cache ü 空 😀/node_modules/item-{index}"),
            &[index, 0xff],
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&ordinary, fs::Permissions::from_mode(0o400)).unwrap();
    }
    let actual = fixture.0.join("actual/node_modules");
    succeeds(&copy(&source, &actual).await);
    let actual_names: BTreeSet<_> = fs::read_dir(&actual)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    let expected_names: BTreeSet<std::ffi::OsString> =
        [".package-lock.json", "@empty", "@scope", "pkg"]
            .into_iter()
            .map(Into::into)
            .chain((0..20).map(|index| format!("item-{index}").into()))
            .collect();
    assert_eq!(actual_names, expected_names);
    assert_eq!(
        fs::read(actual.join("@scope/pkg/deep/value")).unwrap(),
        b"scoped bytes"
    );
    assert!(
        fs::read_dir(actual.join("@empty"))
            .unwrap()
            .next()
            .is_none()
    );
    for (from, to) in [
        (ordinary, actual.join(".package-lock.json")),
        (scoped, actual.join("@scope/ordinary")),
    ] {
        assert_eq!(
            filesystem::stat_identity(&from).unwrap(),
            filesystem::stat_identity(&to).unwrap()
        );
        assert_eq!(
            fs::metadata(from).unwrap().permissions(),
            fs::metadata(to).unwrap().permissions()
        );
    }
    for index in 0..20 {
        assert_eq!(
            fs::read(actual.join(format!("item-{index}"))).unwrap(),
            [index, 0xff]
        );
    }
    fs::write(actual.join("pkg/deep/value"), b"through directory link").unwrap();
    assert_eq!(
        fs::read(source.join("pkg/deep/value")).unwrap(),
        b"through directory link"
    );
    fs::write(actual.join("item-0"), b"through hardlink").unwrap();
    assert_eq!(
        fs::read(source.join("item-0")).unwrap(),
        b"through hardlink"
    );
}

#[tokio::test]
async fn ordered_workspace_restore_replaces_owned_destinations_and_follows_prior_entries() {
    let fixture = Fixture::new();
    let actual = fixture.0.join("actual");
    fixture.write("actual/packages/first/value ü 空 😀", b"workspace bytes");
    fixture.write(
        "actual/node_modules/@scope/first/old",
        b"obsolete destination",
    );
    let readonly = fixture.write("actual/node_modules/second", b"obsolete readonly file");
    let mut permissions = fs::metadata(&readonly).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(readonly, permissions).unwrap();
    let public_name = std::ffi::OsString::from("workspace ü 空 😀");
    #[cfg(windows)]
    let public_name = {
        use std::os::windows::ffi::OsStringExt;
        let mut name = public_name;
        name.push(std::ffi::OsString::from_wide(&[0xd800]));
        name
    };
    let public_location = PathBuf::from("node_modules").join(public_name);
    let public_location_text = ivy_host_runtime::native_path::from_native(&public_location);
    succeeds(
        &preparation_links::restore_workspace_links(
            actual.clone(),
            vec![
                ("node_modules/@scope/first".into(), "packages/first".into()),
                (
                    "node_modules/second".into(),
                    "node_modules/@scope/first".into(),
                ),
                (public_location_text, "packages/first".into()),
            ],
        )
        .await,
    );
    assert_eq!(
        fs::read(actual.join("node_modules/second/value ü 空 😀")).unwrap(),
        b"workspace bytes"
    );
    assert!(!actual.join("node_modules/@scope/first/old").exists());
    assert_eq!(
        fs::read(actual.join(&public_location).join("value ü 空 😀")).unwrap(),
        b"workspace bytes"
    );
    assert_eq!(
        filesystem::stat_identity(&actual.join(&public_location)).unwrap(),
        filesystem::stat_identity(&actual.join("packages/first")).unwrap()
    );
    for path in ["node_modules/@scope/first", "node_modules/second"] {
        assert!(
            fs::symlink_metadata(actual.join(path))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            filesystem::stat_identity(&actual.join(path)).unwrap(),
            filesystem::stat_identity(&actual.join("packages/first")).unwrap()
        );
    }
}

#[tokio::test]
async fn workspace_source_checks_precede_destination_removal_and_stop_in_map_order() {
    let fixture = Fixture::new();
    let outside = fixture.0.join("outside");
    fixture.write("outside/retained", b"outside unchanged");
    let scenarios = [
        ("../escape", "packages/good", "target_conflict"),
        ("node_modules/pkg", "../outside", "target_conflict"),
        ("node_modules/pkg", "packages/file", "artifact_changed"),
        ("node_modules/pkg", "packages/missing", "internal_error"),
        ("node_modules/pkg", "packages/escape", "artifact_changed"),
    ];
    for (index, (location, target, error)) in scenarios.into_iter().enumerate() {
        let actual = fixture.0.join(format!("actual-{index}"));
        fs::create_dir_all(actual.join("packages/good")).unwrap();
        fs::write(actual.join("packages/file"), b"not a directory").unwrap();
        fs::create_dir_all(actual.join("node_modules/pkg")).unwrap();
        fs::write(
            actual.join("node_modules/pkg/retained"),
            b"destination unchanged",
        )
        .unwrap();
        directory_link(&outside, &actual.join("packages/escape"));
        refuses(
            &preparation_links::restore_workspace_links(
                actual.clone(),
                vec![
                    (location.into(), target.into()),
                    ("node_modules/later".into(), "packages/good".into()),
                ],
            )
            .await,
            error,
        );
        assert_eq!(
            fs::read(actual.join("node_modules/pkg/retained")).unwrap(),
            b"destination unchanged"
        );
        assert!(!actual.join("node_modules/later").exists());
    }
    assert_eq!(
        fs::read(outside.join("retained")).unwrap(),
        b"outside unchanged"
    );
}

#[tokio::test]
async fn cache_observed_links_keep_verbatim_targets_on_native_filesystem() {
    let fixture = Fixture::new();
    let source = fixture.0.join("cache");
    let target = fixture.0.join(opaque_name());
    fs::create_dir(&target).unwrap();
    fs::write(target.join("retained"), b"opaque target").unwrap();
    fixture.write("relative-target/retained", b"relative target");
    fs::create_dir(&source).unwrap();
    let name = opaque_name();
    directory_link(&target, &source.join(&name));
    relative_directory_link("../relative-target", &source.join("relative"));
    let expected_target = fs::read_link(source.join(&name)).unwrap();
    let expected_relative = fs::read_link(source.join("relative")).unwrap();
    let actual = fixture.0.join("actual");
    succeeds(&copy(&source, &actual).await);
    assert_eq!(fs::read_link(actual.join(&name)).unwrap(), expected_target);
    assert_eq!(
        fs::read_link(actual.join("relative")).unwrap(),
        expected_relative
    );
    assert_eq!(
        fs::read(actual.join(&name).join("retained")).unwrap(),
        b"opaque target"
    );
    assert_eq!(
        fs::read(actual.join("relative/retained")).unwrap(),
        b"relative target"
    );
    assert!(
        fs::symlink_metadata(actual.join(&name))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(target.join("retained")).unwrap(), b"opaque target");
}

#[tokio::test]
async fn existing_entries_and_missing_sources_keep_error_and_existing_bytes() {
    let fixture = Fixture::new();
    let source = fixture.0.join("cache");
    fixture.write("cache/pkg/value", b"cache bytes");
    fixture.write("actual/pkg", b"existing destination");
    refuses(
        &copy(&source, &fixture.0.join("actual")).await,
        "internal_error",
    );
    assert_eq!(
        fs::read(fixture.0.join("actual/pkg")).unwrap(),
        b"existing destination"
    );
    refuses(
        &copy(
            &fixture.0.join("missing"),
            &fixture.0.join("missing-actual"),
        )
        .await,
        "internal_error",
    );
    assert!(fixture.0.join("missing-actual").is_dir());
}

#[tokio::test]
async fn shared_worker_core_retains_first_failure_and_drains_admitted_work() {
    let started = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let error = concurrency::map_concurrent((0..20).collect(), 3, |item, _| {
        let started = started.clone();
        let finished = finished.clone();
        let barrier = barrier.clone();
        async move {
            started.fetch_add(1, Ordering::SeqCst);
            barrier.wait().await;
            if item == 0 {
                return Err(IvyError::new(
                    "expected_worker_failure",
                    "first retained error",
                ));
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
            finished.fetch_add(1, Ordering::SeqCst);
            Ok(item)
        }
    })
    .await
    .unwrap_err();
    assert_eq!(error.code, "expected_worker_failure");
    assert_eq!(started.load(Ordering::SeqCst), 3);
    assert_eq!(finished.load(Ordering::SeqCst), 2);
    assert_eq!(
        concurrency::map_concurrent((0..17).collect(), 8, |item, _| async move {
            tokio::time::sleep(Duration::from_millis((17 - item) as u64)).await;
            Ok(item * item)
        })
        .await
        .unwrap(),
        (0..17).map(|value| value * value).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn cross_device_file_copy_fallback_uses_a_separately_supplied_native_volume() {
    let Some(temporary) = std::env::var_os("IVY_HOST_CROSS_DEVICE_TMP") else {
        eprintln!(
            "Unavailable: cross-device hardlink fallback needs an explicit existing IVY_HOST_CROSS_DEVICE_TMP fixture."
        );
        assert!(std::env::var_os("IVY_REQUIRE_CROSS_DEVICE").is_none());
        return;
    };
    let fixture = Fixture::new();
    let source = fixture.0.join("cache");
    let file = fixture.write("cache/file", b"cross-device bytes");
    let temporary = PathBuf::from(temporary);
    assert!(temporary.is_absolute() && temporary.is_dir());
    let temporary = fs::canonicalize(temporary).unwrap();
    let destination = temporary.join(fixture.0.file_name().unwrap());
    assert_eq!(destination.parent(), Some(temporary.as_path()));
    fs::create_dir(&destination).unwrap();
    struct Cross {
        path: PathBuf,
        parent: PathBuf,
    }
    impl Drop for Cross {
        fn drop(&mut self) {
            if let Ok(path) = fs::canonicalize(&self.path) {
                if path.parent() == Some(self.parent.as_path()) {
                    let _ = fs::remove_dir_all(path);
                }
            }
        }
    }
    let cross = Cross {
        path: destination,
        parent: temporary,
    };
    let error = fs::hard_link(&file, cross.path.join("must-not-link")).unwrap_err();
    #[cfg(windows)]
    assert_eq!(error.raw_os_error(), Some(17));
    #[cfg(unix)]
    assert_eq!(error.raw_os_error(), Some(libc::EXDEV));
    succeeds(&copy(&source, &cross.path.join("actual")).await);
    assert_eq!(
        fs::read(cross.path.join("actual/file")).unwrap(),
        b"cross-device bytes"
    );
    assert_ne!(
        filesystem::stat_identity(&file).unwrap(),
        filesystem::stat_identity(&cross.path.join("actual/file")).unwrap()
    );
}

#[tokio::test]
async fn copied_links_preserve_existing_target_guards_before_replacement() {
    let fixture = Fixture::new();
    let target = fixture.0.join(opaque_name());
    fs::create_dir_all(target.join("child/nested")).unwrap();
    fs::write(target.join("child/nested/retained"), b"unchanged source").unwrap();
    fixture.write("other/retained", b"unchanged other");
    let neighbor = fixture.0.join("native ü 空 😀; ' �");
    fs::create_dir_all(neighbor.join("child")).unwrap();
    fs::write(
        neighbor.join("child/retained"),
        b"distinct replacement target",
    )
    .unwrap();
    let kinds = [
        "replace",
        "same-target",
        "descendant",
        "ancestor",
        "regular",
        "dangling",
        "dangling-existing",
        "replacement-neighbor",
        "file-link-replace",
        #[cfg(windows)]
        "case-target",
        #[cfg(windows)]
        "ordinary-prefix",
    ];
    for (index, kind) in kinds.into_iter().enumerate() {
        let source = fixture.0.join(format!("cache-{index}"));
        fs::create_dir(&source).unwrap();
        let source_target = if kind.starts_with("dangling") {
            fixture.0.join("absent-target")
        } else if kind == "file-link-replace" {
            target.join("child/nested/retained")
        } else {
            target.join("child")
        };
        if kind == "file-link-replace" {
            file_link(&source_target, &source.join("link"));
        } else {
            directory_link(&source_target, &source.join("link"));
        }
        let actual = fixture.0.join(format!("actual-{index}"));
        fs::create_dir(&actual).unwrap();
        let link = actual.join("link");
        match kind {
            "replace" | "dangling-existing" => directory_link(&fixture.0.join("other"), &link),
            "same-target" => directory_link(&target.join("child"), &link),
            "descendant" | "file-link-replace" => {
                directory_link(&target.join("child/nested"), &link)
            }
            "ancestor" => directory_link(&target, &link),
            "regular" => fs::write(&link, b"existing file").unwrap(),
            "replacement-neighbor" => directory_link(&neighbor.join("child"), &link),
            #[cfg(windows)]
            "case-target" => {
                use std::os::windows::ffi::{OsStrExt, OsStringExt};
                let units: Vec<u16> = source_target
                    .as_os_str()
                    .encode_wide()
                    .map(|unit| {
                        if (65..=90).contains(&unit) {
                            unit + 32
                        } else if (97..=122).contains(&unit) {
                            unit - 32
                        } else {
                            unit
                        }
                    })
                    .collect();
                directory_link(Path::new(&std::ffi::OsString::from_wide(&units)), &link);
            }
            #[cfg(windows)]
            "ordinary-prefix" => {
                use std::os::windows::ffi::{OsStrExt, OsStringExt};
                let units: Vec<_> = source_target.as_os_str().encode_wide().collect();
                assert_eq!(&units[..4], &[92, 92, 63, 92]);
                std::os::windows::fs::symlink_dir(
                    std::ffi::OsString::from_wide(&units[4..]),
                    &link,
                )
                .unwrap();
            }
            _ => (),
        }
        let prior = fs::read_link(&link).ok();
        let result = copy(&source, &actual).await;
        if [
            "same-target",
            "descendant",
            "ancestor",
            "regular",
            "dangling-existing",
            "case-target",
            "ordinary-prefix",
        ]
        .contains(&kind)
        {
            refuses(&result, "internal_error");
            if kind == "regular" {
                assert_eq!(fs::read(&link).unwrap(), b"existing file");
            } else {
                assert_eq!(fs::read_link(&link).unwrap(), prior.unwrap());
            }
        } else {
            succeeds(&result);
            assert!(
                fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(
                fs::read_link(&link).unwrap(),
                fs::read_link(source.join("link")).unwrap()
            );
        }
        assert_eq!(
            fs::read(target.join("child/nested/retained")).unwrap(),
            b"unchanged source"
        );
        assert_eq!(
            fs::read(fixture.0.join("other/retained")).unwrap(),
            b"unchanged other"
        );
        assert_eq!(
            fs::read(neighbor.join("child/retained")).unwrap(),
            b"distinct replacement target"
        );
    }
}
