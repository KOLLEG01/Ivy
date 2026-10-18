use super::{ServiceFixture, assert_json_eq};
use ivy_host_runtime::{IvyJson, IvyResult, node_path, preparation_files};
use ivy_contracts::JsString;
use std::{fs, io::Write, path::{Path, PathBuf}, process::{Command, Stdio}, sync::Mutex};
static CORPUS: Mutex<()> = Mutex::new(());

fn path(value: &Path) -> IvyJson { IvyJson::String(node_path::from_native(value)) }
fn strings(values: &[&str]) -> Vec<JsString> { values.iter().map(|value| JsString::from(*value)).collect() }
fn write(root: &Path, local: &str, bytes: &[u8]) {
    let target = root.join(local); fs::create_dir_all(target.parent().unwrap()).unwrap(); fs::write(target, bytes).unwrap();
}
fn link(from: &Path, to: &Path, directory: bool) {
    fs::create_dir_all(to.parent().unwrap()).unwrap();
    #[cfg(windows)] if directory { std::os::windows::fs::symlink_dir(from, to).unwrap(); } else { std::os::windows::fs::symlink_file(from, to).unwrap(); }
    #[cfg(unix)] { let _ = directory; std::os::unix::fs::symlink(from, to).unwrap(); }
}
struct RestoreTreePermissions(PathBuf);
impl Drop for RestoreTreePermissions {
    fn drop(&mut self) {
        #[cfg(windows)] fn restore(path: &Path) {
            let Ok(metadata) = fs::symlink_metadata(path) else { return; };
            if metadata.file_type().is_symlink() { return; }
            if metadata.is_dir() { if let Ok(entries) = fs::read_dir(path) { for entry in entries.flatten() { restore(&entry.path()); } } }
            if metadata.permissions().readonly() { let mut permissions = metadata.permissions(); permissions.set_readonly(false); let _ = fs::set_permissions(path, permissions); }
        }
        #[cfg(windows)] restore(&self.0);
        #[cfg(not(windows))] let _ = &self.0;
    }
}
fn inventory(root: &Path) -> IvyJson {
    if !root.exists() { return IvyJson::Null; }
    fn walk(root: &Path, local: &JsString, output: &mut Vec<IvyJson>) {
        let mut entries = fs::read_dir(root).unwrap().map(|entry| entry.unwrap()).collect::<Vec<_>>();
        entries.sort_by_key(|entry| node_path::node_utf8(&entry.file_name()));
        for entry in entries {
            let mut units = local.0.clone(); if !units.is_empty() { units.push(47); } units.extend(node_path::node_utf8(&entry.file_name()).0);
            let local = JsString(units); let metadata = fs::symlink_metadata(entry.path()).unwrap();
            let kind = if metadata.file_type().is_symlink() { "link" } else if metadata.is_dir() { "directory" } else { "file" };
            let mut value = IvyJson::object([("path", IvyJson::String(local.clone())), ("type", kind.into())]);
            if metadata.file_type().is_symlink() { value.insert("target", path(&fs::read_link(entry.path()).unwrap())); }
            else if metadata.is_file() { value.insert("hex", fs::read(entry.path()).unwrap().iter().map(|byte| format!("{byte:02x}")).collect::<String>().into()); }
            #[cfg(unix)] if !metadata.file_type().is_symlink() { use std::os::unix::fs::MetadataExt; value.insert("mode", IvyJson::Number((metadata.mode() & 0o777) as f64)); }
            output.push(value);
            if metadata.is_dir() && !metadata.file_type().is_symlink() { walk(&entry.path(), &local, output); }
        }
    }
    let mut output = Vec::new(); walk(root, &"".into(), &mut output); IvyJson::Array(output)
}
fn operation(result: IvyResult<()>) -> IvyJson {
    match result { Ok(()) => IvyJson::object([("ok", true.into()), ("value", IvyJson::Null)]), Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]) }
}
async fn compare(fixture: &ServiceFixture, name: &str, source: &Path, selected: &[&str], dist: &str) -> IvyJson {
    let actual_root = fixture.0.join("actual"); let expected_root = fixture.0.join("original"); let cache = fixture.0.join("cache");
    // prepareCandidate creates artifactRoot before its collection phase.
    if name == "service" { fs::create_dir_all(&actual_root).unwrap(); fs::create_dir_all(&expected_root).unwrap(); }
    let result = match name {
        "source" => preparation_files::copy_source_tree(source.to_owned(), actual_root.clone()).await,
        "runtime" => preparation_files::copy_runtime_dependencies(source.to_owned(), actual_root.clone(), strings(selected), cache.clone()).await,
        "service" => preparation_files::collect_service_files(source.to_owned(), actual_root.clone(), strings(selected), cache.clone()).await,
        "app" => preparation_files::collect_app_bundle(source.to_owned(), actual_root.clone(), dist.into()).await,
        _ => unreachable!(),
    };
    let succeeded = result.is_ok();
    let value = IvyJson::object([("operation", operation(result)), ("tree", if succeeded { inventory(&actual_root) } else { IvyJson::Null })]);
    let input = IvyJson::object([("operation", name.into()), ("source", path(source)), ("destination", path(&expected_root)), ("selected", IvyJson::Array(strings(selected).into_iter().map(IvyJson::String).collect())), ("cache", path(&cache)), ("dist", dist.into())]);
    let mut child = Command::new("node").args(["--experimental-transform-types", "--disable-warning=ExperimentalWarning", "tests/fixtures/host-preparation-files-oracle.mjs"])
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(ivy_contracts::json::stringify(&input, usize::MAX).unwrap().as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap(); assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let expected = ivy_contracts::json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let actual = IvyJson::object([("ok", true.into()), ("value", value.clone())]); assert_json_eq(&actual, &expected);
    if let Some(file) = std::env::var_os("IVY_HOST_PREPARATION_FILES_CORPUS") {
        let _guard = CORPUS.lock().unwrap(); writeln!(fs::OpenOptions::new().create(true).append(true).open(file).unwrap(), "{}", ivy_contracts::json::stringify(&IvyJson::object([("input", input), ("actual", actual), ("expected", expected), ("observedActualTree", inventory(&actual_root)), ("observedOriginalTree", inventory(&expected_root))]), usize::MAX).unwrap()).unwrap();
    }
    value.required("operation").unwrap().clone()
}
fn succeeds(value: &IvyJson) { assert_eq!(value.get("ok"), Some(&IvyJson::Bool(true))); }
fn refuses(value: &IvyJson, code: &str) { assert_eq!(value.required("error").unwrap().required("data").unwrap().get("code"), Some(&IvyJson::from(code))); }

#[tokio::test]
async fn fixed_source_copy_inventories_before_output_and_preserves_bytes_empty_dirs_and_file_modes() {
    let fixture = ServiceFixture::new(); let _restore = RestoreTreePermissions(fixture.0.clone()); let source = fixture.0.join("fixed-source");
    fs::create_dir_all(source.join("empty/nested")).unwrap();
    for index in 0..25 { write(&source, &format!("part-{index}/nested/payload ü 空 😀"), &[0, index, 0xff]); }
    write(&source, "empty-file", b""); write(&source, "executable", b"independent input");
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; fs::set_permissions(source.join("executable"), fs::Permissions::from_mode(0o751)).unwrap(); fs::set_permissions(source.join("part-0"), fs::Permissions::from_mode(0o750)).unwrap(); }
    #[cfg(windows)] { let mut permissions = fs::metadata(source.join("executable")).unwrap().permissions(); permissions.set_readonly(true); fs::set_permissions(source.join("executable"), permissions).unwrap(); }
    succeeds(&compare(&fixture, "source", &source, &[], "").await);
    assert_eq!(fs::read(fixture.0.join("actual/executable")).unwrap(), b"independent input");
    fs::write(source.join("part-1/nested/payload ü 空 😀"), b"changed source").unwrap();
    assert_eq!(fs::read(fixture.0.join("actual/part-1/nested/payload ü 空 😀")).unwrap(), [0, 1, 0xff]);
}

#[tokio::test]
async fn fixed_source_rejects_observed_file_directory_dangling_links_and_special_entries_without_output() {
    for kind in ["file", "directory", "dangling", "missing-root"] {
        let fixture = ServiceFixture::new(); let source = fixture.0.join("source"); fs::create_dir_all(source.join("nested")).unwrap(); write(&source, "regular", b"unchanged");
        let outside = fixture.0.join("outside"); fs::create_dir(&outside).unwrap(); write(&outside, "retained", b"outside");
        if kind == "missing-root" { fs::remove_dir_all(&source).unwrap(); }
        else { let target = if kind == "directory" { outside.clone() } else { outside.join(if kind == "file" { "retained" } else { "missing" }) }; link(&target, &source.join("nested/link"), kind == "directory"); }
        refuses(&compare(&fixture, "source", &source, &[], "").await, if kind == "missing-root" { "internal_error" } else { "artifact_changed" });
        assert!(!fixture.0.join("actual").exists()); assert_eq!(fs::read(outside.join("retained")).unwrap(), b"outside");
    }
    #[cfg(unix)] {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let fixture = ServiceFixture::new(); let source = fixture.0.join("source"); fs::create_dir(&source).unwrap();
        let fifo = CString::new(source.join("fifo").as_os_str().as_bytes()).unwrap(); assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        refuses(&compare(&fixture, "source", &source, &[], "").await, "artifact_changed"); assert!(!fixture.0.join("actual").exists());
    }
}

#[tokio::test]
async fn runtime_copy_materializes_contained_cache_workspace_and_file_links_with_original_filters() {
    let fixture = ServiceFixture::new(); let source = fixture.0.join("source"); let cache = fixture.0.join("cache");
    for local in ["node_modules/a/index.js", "node_modules/a/nested/keep.txt", "node_modules/a/node_modules/transitive/index.js", "node_modules/a/node_modules/also-copied/index.js", "node_modules/unused/index.js", "node_modules/a/.bin/omit", "node_modules/a/.cache/omit", "node_modules/a/.local/omit", "node_modules/a/omit.map", "node_modules/a/directory.map/omit"] { write(&source, local, local.as_bytes()); }
    for index in 0..19 { write(&source, &format!("node_modules/a/branch-{index}/file"), &[index]); }
    write(&cache, "scoped/index.js", b"cached bytes"); write(&cache, "shared/executable", b"cached executable"); write(&source, "packages/workspace/value", b"fresh workspace bytes");
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; fs::set_permissions(cache.join("shared/executable"), fs::Permissions::from_mode(0o751)).unwrap(); }
    link(&cache.join("scoped"), &source.join("node_modules/@scope/pkg"), true);
    link(&cache.join("shared/executable"), &source.join("node_modules/a/nested/tool"), false);
    link(&source.join("packages/workspace"), &source.join("node_modules/a/workspace"), true);
    link(&fixture.0.join("missing"), &source.join("node_modules/absent-platform"), true);
    link(&fixture.0.join("outside"), &source.join("node_modules/a/excluded.map"), true);
    succeeds(&compare(&fixture, "runtime", &source, &["node_modules/a", "node_modules/a/node_modules/transitive", "node_modules/@scope/pkg", "node_modules/absent-platform"], "").await);
    let actual = fixture.0.join("actual/node_modules");
    assert!(!actual.join("unused").exists()); assert!(!actual.join("a/.cache").exists()); assert!(!actual.join("a/directory.map").exists());
    assert_eq!(fs::read(actual.join("a/workspace/value")).unwrap(), b"fresh workspace bytes");
    assert!(fs::symlink_metadata(actual.join("@scope/pkg")).unwrap().is_dir()); assert!(!fs::symlink_metadata(actual.join("@scope/pkg")).unwrap().file_type().is_symlink());
    fs::write(cache.join("scoped/index.js"), b"later cache bytes").unwrap(); assert_eq!(fs::read(actual.join("@scope/pkg/index.js")).unwrap(), b"cached bytes");
}

#[tokio::test]
async fn runtime_copy_distinguishes_missing_roots_from_escaping_or_broken_nested_links() {
    for kind in ["empty-selection", "missing-root", "outside-root", "source-sibling-root", "cache-sibling-root", "outside-nested", "broken-nested", "file-root", "lexical-escape"] {
        let fixture = ServiceFixture::new(); let source = fixture.0.join("source"); fs::create_dir_all(source.join("node_modules")).unwrap();
        let outside = fixture.0.join(if kind == "source-sibling-root" { "source-extra" } else if kind == "cache-sibling-root" { "cache-extra" } else { "outside" }); fs::create_dir(&outside).unwrap(); write(&outside, "retained", b"outside");
        match kind {
            "outside-root" | "source-sibling-root" | "cache-sibling-root" => link(&outside, &source.join("node_modules/a"), true),
            "outside-nested" => { write(&source, "node_modules/a/ordinary", b"not copied after inventory error"); link(&outside, &source.join("node_modules/a/link"), true); }
            "broken-nested" => { fs::create_dir_all(source.join("node_modules/a")).unwrap(); link(&outside.join("missing"), &source.join("node_modules/a/link"), false); }
            "file-root" => write(&source, "node_modules/a", b"not a package directory"),
            _ => {}
        }
        let selected: &[&str] = if kind == "empty-selection" { &[] } else if kind == "lexical-escape" { &["../outside"] } else { &["node_modules/a"] };
        let result = compare(&fixture, "runtime", &source, selected, "").await;
        if ["empty-selection", "missing-root"].contains(&kind) { succeeds(&result); assert!(!fixture.0.join("actual").exists()); }
        else { refuses(&result, if ["broken-nested", "file-root"].contains(&kind) { "internal_error" } else { "artifact_changed" }); }
        assert_eq!(fs::read(outside.join("retained")).unwrap(), b"outside");
    }
}

#[tokio::test]
async fn service_collection_requires_its_outputs_and_filters_before_resolving_excluded_links() {
    for kind in ["ordinary", "missing-dist", "missing-package", "missing-lock", "outside-output"] {
        let fixture = ServiceFixture::new(); let source = fixture.0.join("source"); fs::create_dir_all(source.join("dist")).unwrap();
        write(&source, "package.json", b"{}"); write(&source, "package-lock.json", b"{}"); write(&source, "dist/main.js", b"entrypoint"); write(&source, "dist/tests/included", b"dist tests are not root tests");
        for local in ["dist/omit.map", "dist/.cache/omit", "dist/.bin/omit", "dist/.local/omit"] { write(&source, local, b"excluded"); }
        write(&source, "shared/value", b"shared output"); link(&source.join("shared"), &source.join("dist/contained"), true);
        link(&fixture.0.join("missing"), &source.join("dist/unsafe.map"), false);
        if kind == "missing-dist" { fs::remove_dir_all(source.join("dist")).unwrap(); }
        else if kind == "missing-package" { fs::remove_file(source.join("package.json")).unwrap(); }
        else if kind == "missing-lock" { fs::remove_file(source.join("package-lock.json")).unwrap(); }
        else if kind == "outside-output" { let outside = fixture.0.join("outside"); write(&outside, "retained", b"outside"); link(&outside, &source.join("dist/leaves-output"), true); }
        let result = compare(&fixture, "service", &source, &[], "").await;
        if kind == "ordinary" { succeeds(&result); assert_eq!(fs::read(fixture.0.join("actual/dist/contained/value")).unwrap(), b"shared output"); }
        else { refuses(&result, if kind == "outside-output" { "artifact_changed" } else { "artifact_missing" }); }
    }
}

#[tokio::test]
async fn app_collection_uses_exact_selected_bundle_containment_and_resolves_links_before_map_filtering() {
    for kind in ["ordinary", "missing", "file", "escaping-dist", "outside-root", "outside-nested", "outside-map", "broken-map"] {
        let fixture = ServiceFixture::new(); let source = fixture.0.join("source"); let bundle = source.join("dist/apps/fixture"); fs::create_dir_all(&bundle).unwrap();
        write(&bundle, "index.html", b"selected app"); write(&bundle, "assets/value ü 空 😀", b"asset"); write(&bundle, ".cache/retained", b"app cache is not excluded"); write(&bundle, "safe.map", b"excluded source map");
        link(&bundle.join("assets"), &bundle.join("copied-assets"), true);
        let outside = fixture.0.join("outside"); write(&outside, "retained", b"outside");
        match kind {
            "missing" => fs::remove_dir_all(&bundle).unwrap(),
            "file" => { fs::remove_dir_all(&bundle).unwrap(); write(&source, "dist/apps/fixture", b"not a directory"); }
            "outside-root" => { fs::remove_dir_all(&bundle).unwrap(); link(&outside, &bundle, true); }
            "outside-nested" => link(&outside, &bundle.join("external"), true),
            "outside-map" => link(&outside.join("retained"), &bundle.join("external.map"), false),
            "broken-map" => link(&outside.join("missing"), &bundle.join("broken.map"), false),
            _ => {}
        }
        let dist = if kind == "escaping-dist" { "../outside" } else { "dist/apps/fixture" };
        let result = compare(&fixture, "app", &source, &[], dist).await;
        if kind == "ordinary" { succeeds(&result); assert_eq!(fs::read(fixture.0.join("actual/dist/apps/fixture/copied-assets/value ü 空 😀")).unwrap(), b"asset"); }
        else { refuses(&result, if ["missing", "broken-map"].contains(&kind) { "internal_error" } else if ["file", "escaping-dist", "outside-root"].contains(&kind) { "artifact_missing" } else { "artifact_changed" }); }
        assert_eq!(fs::read(outside.join("retained")).unwrap(), b"outside");
    }
}
