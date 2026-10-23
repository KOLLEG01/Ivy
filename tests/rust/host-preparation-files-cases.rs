use super::ServiceFixture;
use ivy_contracts::JsString;
use ivy_host_runtime::{IvyResult, preparation_files};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

fn strings(values: &[&str]) -> Vec<JsString> {
    values.iter().map(|value| JsString::from(*value)).collect()
}
fn write(root: &Path, local: &str, bytes: &[u8]) {
    let target = root.join(local);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(target, bytes).unwrap();
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
fn source_root(fixture: &ServiceFixture) -> PathBuf {
    fs::canonicalize(&fixture.0).unwrap().join(opaque_name())
}
fn link(from: &Path, to: &Path, directory: bool) {
    fs::create_dir_all(to.parent().unwrap()).unwrap();
    #[cfg(windows)]
    if directory {
        std::os::windows::fs::symlink_dir(from, to).unwrap();
    } else {
        std::os::windows::fs::symlink_file(from, to).unwrap();
    }
    #[cfg(unix)]
    {
        let _ = directory;
        std::os::unix::fs::symlink(from, to).unwrap();
    }
}
struct RestoreTreePermissions(PathBuf);
impl Drop for RestoreTreePermissions {
    fn drop(&mut self) {
        #[cfg(windows)]
        fn restore(path: &Path) {
            let Ok(metadata) = fs::symlink_metadata(path) else {
                return;
            };
            if metadata.file_type().is_symlink() {
                return;
            }
            if metadata.is_dir() {
                if let Ok(entries) = fs::read_dir(path) {
                    for entry in entries.flatten() {
                        restore(&entry.path());
                    }
                }
            }
            if metadata.permissions().readonly() {
                let mut permissions = metadata.permissions();
                permissions.set_readonly(false);
                let _ = fs::set_permissions(path, permissions);
            }
        }
        #[cfg(windows)]
        restore(&self.0);
        #[cfg(not(windows))]
        let _ = &self.0;
    }
}
type Tree = BTreeMap<PathBuf, Option<Vec<u8>>>;
fn tree(root: &Path) -> Tree {
    fn walk(root: &Path, local: &Path, output: &mut Tree) {
        for entry in fs::read_dir(root.join(local)).unwrap() {
            let entry = entry.unwrap();
            let local = local.join(entry.file_name());
            let kind = entry.file_type().unwrap();
            assert!(
                !kind.is_symlink(),
                "copy retained a filesystem link: {local:?}"
            );
            if kind.is_dir() {
                output.insert(local.clone(), None);
                walk(root, &local, output);
            } else {
                assert!(kind.is_file());
                output.insert(local, Some(fs::read(entry.path()).unwrap()));
            }
        }
    }
    let mut output = Tree::new();
    walk(root, Path::new(""), &mut output);
    output
}
fn expected_tree(
    files: impl IntoIterator<Item = (PathBuf, Vec<u8>)>,
    directories: &[&str],
) -> Tree {
    let mut tree = Tree::new();
    for path in directories.iter().map(PathBuf::from) {
        tree.insert(path, None);
    }
    for (path, bytes) in files {
        let mut parent = path.parent();
        while let Some(directory) = parent.filter(|path| !path.as_os_str().is_empty()) {
            tree.insert(directory.to_owned(), None);
            parent = directory.parent();
        }
        tree.insert(path, Some(bytes));
    }
    tree
}
async fn run(
    fixture: &ServiceFixture,
    name: &str,
    source: &Path,
    selected: &[&str],
    dist: &str,
) -> IvyResult<()> {
    let artifact = fs::canonicalize(&fixture.0).unwrap().join("actual");
    let cache = fixture.0.join("cache");
    if name == "service" {
        fs::create_dir_all(&artifact).unwrap();
    }
    match name {
        "source" => preparation_files::copy_source_tree(source.to_owned(), artifact).await,
        "runtime" => {
            preparation_files::copy_runtime_dependencies(
                source.to_owned(),
                artifact,
                strings(selected),
                cache,
            )
            .await
        }
        "service" => {
            preparation_files::collect_service_files(
                source.to_owned(),
                artifact,
                strings(selected),
                cache,
            )
            .await
        }
        "app" => {
            preparation_files::collect_app_bundle(source.to_owned(), artifact, dist.into()).await
        }
        _ => unreachable!(),
    }
}
fn succeeds(value: &IvyResult<()>) {
    assert!(value.is_ok(), "{value:?}");
}
fn refuses(value: &IvyResult<()>, code: &str) {
    assert_eq!(value.as_ref().unwrap_err().code, code);
}
#[tokio::test]
async fn fixed_source_copy_inventories_before_output_and_preserves_bytes_empty_dirs_and_file_modes()
{
    let fixture = ServiceFixture::new();
    let _restore = RestoreTreePermissions(fixture.0.clone());
    let source = source_root(&fixture);
    fs::create_dir_all(source.join("empty/nested")).unwrap();
    for index in 0..25 {
        write(
            &source,
            &format!("part-{index}/nested/payload ü 空 😀"),
            &[0, index, 0xff],
        );
    }
    write(&source, "empty-file", b"");
    write(&source, "executable", b"independent input");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(source.join("executable"), fs::Permissions::from_mode(0o751)).unwrap();
        fs::set_permissions(source.join("part-0"), fs::Permissions::from_mode(0o750)).unwrap();
    }
    #[cfg(windows)]
    {
        let mut permissions = fs::metadata(source.join("executable"))
            .unwrap()
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(source.join("executable"), permissions).unwrap();
    }
    fs::write(source.join(opaque_name()), b"opaque filename").unwrap();
    fs::write(
        source.join("native ü 空 😀; ' �"),
        b"distinct replacement neighbor",
    )
    .unwrap();
    let expected = tree(&source);
    succeeds(&run(&fixture, "source", &source, &[], "").await);
    assert_eq!(tree(&fixture.0.join("actual")), expected);
    assert_eq!(
        fs::metadata(source.join("executable"))
            .unwrap()
            .permissions(),
        fs::metadata(fixture.0.join("actual/executable"))
            .unwrap()
            .permissions()
    );
    assert_eq!(
        fs::read(fixture.0.join("actual/executable")).unwrap(),
        b"independent input"
    );
    fs::write(
        source.join("part-1/nested/payload ü 空 😀"),
        b"changed source",
    )
    .unwrap();
    assert_eq!(
        fs::read(fixture.0.join("actual/part-1/nested/payload ü 空 😀")).unwrap(),
        [0, 1, 0xff]
    );
}

#[tokio::test]
async fn fixed_source_rejects_observed_file_directory_dangling_links_and_special_entries_without_output()
 {
    for kind in ["file", "directory", "dangling", "missing-root", "file-root"] {
        let fixture = ServiceFixture::new();
        let source = source_root(&fixture);
        fs::create_dir_all(source.join("nested")).unwrap();
        write(&source, "regular", b"unchanged");
        let outside = fixture.0.join("outside");
        fs::create_dir(&outside).unwrap();
        write(&outside, "retained", b"outside");
        if kind == "missing-root" {
            fs::remove_dir_all(&source).unwrap();
        } else if kind == "file-root" {
            fs::remove_dir_all(&source).unwrap();
            fs::write(&source, b"not a directory").unwrap();
        } else {
            let target = if kind == "directory" {
                outside.clone()
            } else {
                outside.join(if kind == "file" {
                    "retained"
                } else {
                    "missing"
                })
            };
            link(&target, &source.join("nested/link"), kind == "directory");
        }
        refuses(
            &run(&fixture, "source", &source, &[], "").await,
            if ["missing-root", "file-root"].contains(&kind) {
                "internal_error"
            } else {
                "artifact_changed"
            },
        );
        assert!(!fixture.0.join("actual").exists());
        assert_eq!(fs::read(outside.join("retained")).unwrap(), b"outside");
    }
    #[cfg(unix)]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let fixture = ServiceFixture::new();
        let source = source_root(&fixture);
        fs::create_dir(&source).unwrap();
        let fifo = CString::new(source.join("fifo").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        refuses(
            &run(&fixture, "source", &source, &[], "").await,
            "artifact_changed",
        );
        assert!(!fixture.0.join("actual").exists());
    }
}

#[tokio::test]
async fn runtime_copy_materializes_contained_cache_workspace_and_file_links_with_original_filters()
{
    let fixture = ServiceFixture::new();
    let source = source_root(&fixture);
    let cache = fixture.0.join("cache");
    let mut expected = [
        "node_modules/a/index.js",
        "node_modules/a/nested/keep.txt",
        "node_modules/a/node_modules/transitive/index.js",
        "node_modules/a/node_modules/also-copied/index.js",
    ]
    .map(|local| (PathBuf::from(local), local.as_bytes().to_vec()))
    .to_vec();
    for local in [
        "node_modules/a/index.js",
        "node_modules/a/nested/keep.txt",
        "node_modules/a/node_modules/transitive/index.js",
        "node_modules/a/node_modules/also-copied/index.js",
        "node_modules/unused/index.js",
        "node_modules/a/.bin/omit",
        "node_modules/a/.cache/omit",
        "node_modules/a/.local/omit",
        "node_modules/a/omit.map",
        "node_modules/a/directory.map/omit",
    ] {
        write(&source, local, local.as_bytes());
    }
    for index in 0..19 {
        let local = format!("node_modules/a/branch-{index}/file");
        write(&source, &local, &[index]);
        expected.push((local.into(), vec![index]));
    }
    let opaque = PathBuf::from("node_modules/a").join(opaque_name());
    fs::write(source.join(&opaque), b"opaque dependency").unwrap();
    expected.push((opaque, b"opaque dependency".to_vec()));
    let neighbor = "node_modules/a/native ü 空 😀; ' �";
    write(&source, neighbor, b"distinct dependency");
    expected.push((neighbor.into(), b"distinct dependency".to_vec()));
    write(&cache, "scoped/index.js", b"cached bytes");
    write(&cache, "shared/executable", b"cached executable");
    write(
        &source,
        "packages/workspace/value",
        b"fresh workspace bytes",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            cache.join("shared/executable"),
            fs::Permissions::from_mode(0o751),
        )
        .unwrap();
    }
    link(
        &cache.join("scoped"),
        &source.join("node_modules/@scope/pkg"),
        true,
    );
    link(
        &cache.join("shared/executable"),
        &source.join("node_modules/a/nested/tool"),
        false,
    );
    link(
        &source.join("packages/workspace"),
        &source.join("node_modules/a/workspace"),
        true,
    );
    link(
        &fixture.0.join("missing"),
        &source.join("node_modules/absent-platform"),
        true,
    );
    link(
        &fixture.0.join("outside"),
        &source.join("node_modules/a/excluded.map"),
        true,
    );
    succeeds(
        &run(
            &fixture,
            "runtime",
            &source,
            &[
                "node_modules/a",
                "node_modules/a/node_modules/transitive",
                "node_modules/@scope/pkg",
                "node_modules/absent-platform",
            ],
            "",
        )
        .await,
    );
    for (local, bytes) in [
        (
            "node_modules/@scope/pkg/index.js",
            b"cached bytes".as_slice(),
        ),
        ("node_modules/a/nested/tool", b"cached executable"),
        ("node_modules/a/workspace/value", b"fresh workspace bytes"),
    ] {
        expected.push((local.into(), bytes.to_vec()));
    }
    assert_eq!(
        tree(&fixture.0.join("actual")),
        expected_tree(expected, &[])
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(fixture.0.join("actual/node_modules/a/nested/tool"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o751
        );
    }
    let actual = fixture.0.join("actual/node_modules");
    assert!(!actual.join("unused").exists());
    assert!(!actual.join("a/.cache").exists());
    assert!(!actual.join("a/directory.map").exists());
    assert_eq!(
        fs::read(actual.join("a/workspace/value")).unwrap(),
        b"fresh workspace bytes"
    );
    assert!(
        fs::symlink_metadata(actual.join("@scope/pkg"))
            .unwrap()
            .is_dir()
    );
    assert!(
        !fs::symlink_metadata(actual.join("@scope/pkg"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::write(cache.join("scoped/index.js"), b"later cache bytes").unwrap();
    assert_eq!(
        fs::read(actual.join("@scope/pkg/index.js")).unwrap(),
        b"cached bytes"
    );
}

#[tokio::test]
async fn runtime_copy_distinguishes_missing_roots_from_escaping_or_broken_nested_links() {
    for kind in [
        "empty-selection",
        "missing-root",
        "outside-root",
        "source-sibling-root",
        "cache-sibling-root",
        "outside-nested",
        "broken-nested",
        "file-root",
        "lexical-escape",
    ] {
        let fixture = ServiceFixture::new();
        let source = source_root(&fixture);
        fs::create_dir_all(source.join("node_modules")).unwrap();
        let outside = if kind == "source-sibling-root" {
            let mut sibling = source.as_os_str().to_owned();
            sibling.push("-extra");
            PathBuf::from(sibling)
        } else {
            fixture.0.join(if kind == "cache-sibling-root" {
                "cache-extra"
            } else {
                "outside"
            })
        };
        fs::create_dir(&outside).unwrap();
        write(&outside, "retained", b"outside");
        match kind {
            "outside-root" | "source-sibling-root" | "cache-sibling-root" => {
                link(&outside, &source.join("node_modules/a"), true)
            }
            "outside-nested" => {
                write(
                    &source,
                    "node_modules/a/ordinary",
                    b"not copied after inventory error",
                );
                link(&outside, &source.join("node_modules/a/link"), true);
            }
            "broken-nested" => {
                fs::create_dir_all(source.join("node_modules/a")).unwrap();
                link(
                    &outside.join("missing"),
                    &source.join("node_modules/a/link"),
                    false,
                );
            }
            "file-root" => write(&source, "node_modules/a", b"not a package directory"),
            _ => {}
        }
        let selected: &[&str] = if kind == "empty-selection" {
            &[]
        } else if kind == "lexical-escape" {
            &["../outside"]
        } else {
            &["node_modules/a"]
        };
        let result = run(&fixture, "runtime", &source, selected, "").await;
        if ["empty-selection", "missing-root"].contains(&kind) {
            succeeds(&result);
            assert!(!fixture.0.join("actual").exists());
        } else {
            refuses(
                &result,
                if ["broken-nested", "file-root"].contains(&kind) {
                    "internal_error"
                } else {
                    "artifact_changed"
                },
            );
        }
        assert_eq!(fs::read(outside.join("retained")).unwrap(), b"outside");
    }
}

#[tokio::test]
async fn service_collection_requires_its_outputs_and_filters_before_resolving_excluded_links() {
    for kind in [
        "ordinary",
        "missing-dist",
        "missing-package",
        "missing-lock",
        "outside-output",
    ] {
        let fixture = ServiceFixture::new();
        let source = source_root(&fixture);
        fs::create_dir_all(source.join("dist")).unwrap();
        write(&source, "package.json", b"{}");
        write(&source, "package-lock.json", b"{}");
        write(&source, "dist/main.js", b"entrypoint");
        write(
            &source,
            "dist/tests/included",
            b"dist tests are not root tests",
        );
        for local in [
            "dist/omit.map",
            "dist/.cache/omit",
            "dist/.bin/omit",
            "dist/.local/omit",
        ] {
            write(&source, local, b"excluded");
        }
        write(&source, "shared/value", b"shared output");
        link(&source.join("shared"), &source.join("dist/contained"), true);
        link(
            &fixture.0.join("missing"),
            &source.join("dist/unsafe.map"),
            false,
        );
        if kind == "missing-dist" {
            fs::remove_dir_all(source.join("dist")).unwrap();
        } else if kind == "missing-package" {
            fs::remove_file(source.join("package.json")).unwrap();
        } else if kind == "missing-lock" {
            fs::remove_file(source.join("package-lock.json")).unwrap();
        } else if kind == "outside-output" {
            let outside = fixture.0.join("outside");
            write(&outside, "retained", b"outside");
            link(&outside, &source.join("dist/leaves-output"), true);
        }
        let result = run(&fixture, "service", &source, &[], "").await;
        if kind == "ordinary" {
            succeeds(&result);
            assert_eq!(
                tree(&fixture.0.join("actual")),
                expected_tree(
                    [
                        ("package.json", b"{}".as_slice()),
                        ("package-lock.json", b"{}"),
                        ("dist/main.js", b"entrypoint"),
                        ("dist/tests/included", b"dist tests are not root tests"),
                        ("dist/contained/value", b"shared output")
                    ]
                    .map(|(path, bytes)| (path.into(), bytes.to_vec())),
                    &[]
                )
            );
        } else {
            refuses(
                &result,
                if kind == "outside-output" {
                    "artifact_changed"
                } else {
                    "artifact_missing"
                },
            );
        }
    }
}

#[tokio::test]
async fn app_collection_uses_exact_selected_bundle_containment_and_resolves_links_before_map_filtering()
 {
    for kind in [
        "ordinary",
        "missing",
        "file",
        "escaping-dist",
        "outside-root",
        "outside-nested",
        "outside-map",
        "broken-map",
    ] {
        let fixture = ServiceFixture::new();
        let source = source_root(&fixture);
        let bundle = source.join("dist/apps/fixture");
        fs::create_dir_all(&bundle).unwrap();
        write(&bundle, "index.html", b"selected app");
        write(&bundle, "assets/value ü 空 😀", b"asset");
        write(&bundle, ".cache/retained", b"app cache is not excluded");
        write(&bundle, "safe.map", b"excluded source map");
        link(&bundle.join("assets"), &bundle.join("copied-assets"), true);
        let outside = fixture.0.join("outside");
        write(&outside, "retained", b"outside");
        match kind {
            "missing" => fs::remove_dir_all(&bundle).unwrap(),
            "file" => {
                fs::remove_dir_all(&bundle).unwrap();
                write(&source, "dist/apps/fixture", b"not a directory");
            }
            "outside-root" => {
                fs::remove_dir_all(&bundle).unwrap();
                link(&outside, &bundle, true);
            }
            "outside-nested" => link(&outside, &bundle.join("external"), true),
            "outside-map" => link(
                &outside.join("retained"),
                &bundle.join("external.map"),
                false,
            ),
            "broken-map" => link(&outside.join("missing"), &bundle.join("broken.map"), false),
            _ => {}
        }
        let dist = if kind == "escaping-dist" {
            "../outside"
        } else {
            "dist/apps/fixture"
        };
        let result = run(&fixture, "app", &source, &[], dist).await;
        if kind == "ordinary" {
            succeeds(&result);
            assert_eq!(
                tree(&fixture.0.join("actual")),
                expected_tree(
                    [
                        ("dist/apps/fixture/index.html", b"selected app".as_slice()),
                        ("dist/apps/fixture/assets/value ü 空 😀", b"asset"),
                        ("dist/apps/fixture/copied-assets/value ü 空 😀", b"asset"),
                        (
                            "dist/apps/fixture/.cache/retained",
                            b"app cache is not excluded"
                        )
                    ]
                    .map(|(path, bytes)| (path.into(), bytes.to_vec())),
                    &[]
                )
            );
        } else {
            refuses(
                &result,
                if ["missing", "broken-map"].contains(&kind) {
                    "internal_error"
                } else if ["file", "escaping-dist", "outside-root"].contains(&kind) {
                    "artifact_missing"
                } else {
                    "artifact_changed"
                },
            );
        }
        assert_eq!(fs::read(outside.join("retained")).unwrap(), b"outside");
    }
}

#[test]
fn preparation_copy_admission_survives_dropped_waiters_for_all_payload_kinds() {
    use std::{future::Future, task::Poll, time::Duration};
    for name in ["source", "runtime", "service", "app"] {
        let fixture = ServiceFixture::new();
        let source = source_root(&fixture);
        for (path, bytes) in [
            ("package.json", b"{}".as_slice()),
            ("package-lock.json", b"{}"),
            ("dist/apps/fixture/index.html", b"ui"),
            ("node_modules/a/index.js", b"dependency"),
        ] {
            write(&source, path, bytes);
        }
        let expected = match name {
            "source" | "service" => tree(&source),
            "runtime" => expected_tree(
                [(
                    PathBuf::from("node_modules/a/index.js"),
                    b"dependency".to_vec(),
                )],
                &[],
            ),
            "app" => expected_tree(
                [(
                    PathBuf::from("dist/apps/fixture/index.html"),
                    b"ui".to_vec(),
                )],
                &[],
            ),
            _ => unreachable!(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let (entered, waiting) = tokio::sync::oneshot::channel();
            let (release, hold) = std::sync::mpsc::channel();
            let gate = tokio::task::spawn_blocking(move || {
                let _ = entered.send(());
                hold.recv().unwrap();
            });
            waiting.await.unwrap();
            let mut copy = Box::pin(run(
                &fixture,
                name,
                &source,
                &["node_modules/a"],
                "dist/apps/fixture",
            ));
            std::future::poll_fn(|context| match copy.as_mut().poll(context) {
                Poll::Pending => Poll::Ready(()),
                Poll::Ready(_) => panic!("{name} passed a held blocking worker"),
            })
            .await;
            drop(copy);
            release.send(()).unwrap();
            gate.await.unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let root = fixture.0.join("actual");
                    if root.is_dir()
                        && expected.iter().all(|(path, bytes)| match bytes {
                            Some(bytes) => {
                                fs::read(root.join(path)).is_ok_and(|found| found == *bytes)
                            }
                            None => root.join(path).is_dir(),
                        })
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        });
        assert_eq!(tree(&fixture.0.join("actual")), expected);
    }
}
