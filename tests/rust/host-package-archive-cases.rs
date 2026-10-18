use super::{ServiceFixture, assert_json_eq, compare_config_oracle};
use ivy_host_runtime::{IvyJson, IvyResult, artifact, config, package_archive};
use std::{collections::BTreeMap, fs, io::Write, path::{Path, PathBuf}, process::{Command, Stdio}, sync::Mutex};
static CORPUS: Mutex<()> = Mutex::new(());
struct ReadonlyCleanup(PathBuf);
impl Drop for ReadonlyCleanup {
    fn drop(&mut self) {
        fn clear(path: &Path) {
            let Ok(metadata) = fs::symlink_metadata(path) else { return };
            if metadata.file_type().is_symlink() { return; }
            if metadata.is_dir() { for entry in fs::read_dir(path).unwrap().flatten() { clear(&entry.path()); } }
            else if metadata.permissions().readonly() {
                let mut permissions = metadata.permissions();
                #[cfg(windows)] permissions.set_readonly(false);
                #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; permissions.set_mode(permissions.mode() | 0o200); }
                let _ = fs::set_permissions(path, permissions);
            }
        }
        assert!(self.0.starts_with(std::env::temp_dir())); clear(&self.0);
    }
}
fn path(path: &Path) -> IvyJson { path.to_str().unwrap().into() }
fn record(case: &str, value: IvyJson) {
    if let Some(file) = std::env::var_os("IVY_HOST_PACKAGE_ARCHIVE_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        writeln!(fs::OpenOptions::new().create(true).append(true).open(file).unwrap(), "{}",
            ivy_contracts::json::stringify(&IvyJson::object([("case", case.into()), ("value", value)]), usize::MAX).unwrap()).unwrap();
    }
}
fn compare(operation: &str, args: Vec<IvyJson>, result: IvyResult<IvyJson>) {
    record(operation, match &result { Ok(value) => IvyJson::object([("ok", true.into()), ("value", value.clone())]),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]) });
    compare_config_oracle(IvyJson::object([("kind", "packageArchive".into()), ("operation", operation.into()), ("args", IvyJson::Array(args))]), result);
}
fn helper(input: IvyJson) -> IvyJson {
    let mut child = Command::new("node").arg("tests/fixtures/host-native-package-fixture.mjs")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")).stdin(Stdio::piped())
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(ivy_contracts::json::stringify(&input, usize::MAX).unwrap().as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    ivy_contracts::json::parse(&String::from_utf8(output.stdout).unwrap(), usize::MAX).unwrap()
}
async fn fixture(component: &str, app: bool) -> (ServiceFixture, IvyJson, IvyJson) {
    let (fixture, mut candidate, mut manifest) = super::artifact_cases::fixture().await;
    let package = fixture.0.join("package"); let root = package.join("artifact");
    fs::create_dir_all(root.join("dist/apps/fixture")).unwrap();
    manifest.insert("componentId", component.into()); candidate.insert("componentId", component.into());
    candidate.insert("artifactRoot", path(&root)); candidate.insert("manifestPath", path(&package.join("component.json")));
    if !app {
        manifest.insert("kind", "service".into()); manifest.remove("app");
        let command = |name: &str| IvyJson::object([("executable", "node".into()), ("args", IvyJson::Array(vec![name.into()])), ("timeoutMs", IvyJson::Number(10000.0))]);
        manifest.insert("entrypoint", command("dist/main.js"));
        manifest.insert("readiness", IvyJson::object([("timeoutMs", IvyJson::Number(10000.0)), ("command", command("dist/health.js"))]));
        manifest.insert("shutdown", IvyJson::object([("timeoutMs", IvyJson::Number(5000.0))]));
        manifest.insert("restart", IvyJson::object([("policy", "never".into()), ("minimumDelayMs", IvyJson::Number(100.0)), ("maximumDelayMs", IvyJson::Number(1000.0))]));
    }
    fs::write(root.join("dist/main.js"), b"throw Error('archive validation must not execute JS');").unwrap();
    fs::write(root.join("dist/health.js"), b"throw Error('archive validation must not execute health');").unwrap();
    fs::write(root.join("dist/apps/fixture/index.html"), b"<p>fixture</p>").unwrap();
    fs::write(root.join("dist/read me #=data.txt"), b"\0\xff\xfe\n preserved bytes").unwrap();
    fs::write(root.join("dist").join(format!("{}-data.txt", "long-".repeat(22))), b"long name").unwrap();
    config::atomic_json(package.join("component.json"), manifest.clone()).await.unwrap();
    config::atomic_json(root.join("dist/build-info.json"), IvyJson::object([("schemaVersion", IvyJson::Number(1.0)),
        ("componentId", component.into()), ("version", manifest.required("version").unwrap().clone()),
        ("buildId", candidate.required("buildId").unwrap().clone())])).await.unwrap();
    (fixture, candidate, manifest)
}
fn authorization(manifest: &IvyJson) -> IvyJson {
    IvyJson::object([("componentId", manifest.required("componentId").unwrap().clone()), ("version", manifest.required("version").unwrap().clone()),
        ("buildId", manifest.required("buildId").unwrap().clone()), ("archiveHash", format!("sha256:{}", "b".repeat(64)).into()),
        ("bytes", IvyJson::Number(1.0)), ("manifest", manifest.clone()), ("extra", "ignored".into())])
}

#[tokio::test]
async fn package_authorization_and_identity_first_index_match_original_limits_and_projection() {
    let (_fixture, _, manifest) = fixture("fixture", true).await;
    let good = authorization(&manifest);
    let mut cases = vec![IvyJson::Null, IvyJson::Array(vec![]), good.clone()];
    for (key, value) in [("componentId", "bad_id".into()), ("componentId", "different".into()), ("version", "01.0.0".into()),
        ("version", IvyJson::String(ivy_contracts::JsString(vec![0xd800]))), ("buildId", "sha256:bad".into()),
        ("archiveHash", format!("sha256:{}", "A".repeat(64)).into()), ("bytes", IvyJson::Number(0.0)),
        ("bytes", IvyJson::Number(1.5)), ("bytes", IvyJson::Number(package_archive::MAXIMUM_ARCHIVE_BYTES as f64)),
        ("bytes", IvyJson::Number(package_archive::MAXIMUM_ARCHIVE_BYTES as f64 + 1.0)), ("bytes", "1".into())] {
        let mut value_case = good.clone(); value_case.insert(key, value); cases.push(value_case);
    }
    for value in cases { compare("validatePackageAuthorization", vec![value.clone()], package_archive::validate_package_authorization(&value)); }
    let good = IvyJson::object([("schemaVersion", IvyJson::Number(1.0)), ("revision", IvyJson::Number(0.0)),
        ("packages", IvyJson::Array(vec![IvyJson::object([("componentId", "fixture".into()), ("version", "999999999999999999999999.0.0".into()),
            ("manifest", IvyJson::object([("schemaVersion", IvyJson::Number(999.0))]))])])), ("extension", true.into())]);
    let mut cases = vec![good.clone(), IvyJson::Null];
    for (key, value) in [("schemaVersion", IvyJson::Number(2.0)), ("revision", IvyJson::Number(-1.0)),
        ("revision", IvyJson::Number(9_007_199_254_740_992.0)), ("packages", IvyJson::Array(vec![IvyJson::Null])),
        ("packages", IvyJson::Array(vec![IvyJson::object([("componentId", "fixture".into()), ("version", "1.0.0-beta".into())])])),
        ("packages", IvyJson::Array(vec![IvyJson::object([("componentId", "fixture".into()), ("version", "1.0.0".into())]); 4097]))] {
        let mut value_case = good.clone(); value_case.insert(key, value); cases.push(value_case);
    }
    for value in cases { compare("validatePackageCatalogIndex", vec![value.clone()], package_archive::validate_package_catalog_index(&value)); }
    let mut entry = authorization(&manifest); entry.insert("revision", IvyJson::Number(1.0));
    entry.insert("publishedAt", "2026-10-18T00:00:00.000Z".into()); entry.insert("publisherPrincipalId", "publisher".into());
    let catalog = |entry| IvyJson::object([("schemaVersion", IvyJson::Number(1.0)), ("revision", IvyJson::Number(1.0)), ("packages", IvyJson::Array(vec![entry]))]);
    let mut cases = vec![catalog(entry.clone())];
    for (key, value) in [("revision", IvyJson::Number(0.0)), ("revision", IvyJson::Number(1.5)),
        ("publishedAt", "February 30, 2024".into()), ("publishedAt", "0".into()), ("publishedAt", "invalid".into()),
        ("publishedAt", IvyJson::Number(0.0)), ("publishedAt", "2026-10-18T12:34:56+02:00".into()),
        ("publisherPrincipalId", "".into()), ("publisherPrincipalId", "a".repeat(256).into()), ("publisherPrincipalId", "a".repeat(257).into())] {
        let mut value_case = entry.clone(); value_case.insert(key, value); cases.push(catalog(value_case));
    }
    for value in cases { compare("validatePackageCatalog", vec![value.clone()], package_archive::validate_package_catalog(&value)); }
}

#[tokio::test]
async fn ordinary_archives_interoperate_with_original_receivers_and_preserve_files_and_modes() {
    for app in [true, false] {
        let (fixture, candidate, manifest) = fixture("fixture", app).await;
        let root = PathBuf::from(candidate.required("artifactRoot").unwrap().as_str().unwrap());
        let archive = package_archive::create_package_archive(candidate.clone(), BTreeMap::new()).await.unwrap();
        let saved = fixture.0.join("rust.tar.gz"); fs::copy(&archive.path, &saved).unwrap();
        let extraction = fixture.0.join("extracted"); fs::create_dir(&extraction).unwrap(); fs::write(extraction.join("stale"), b"discard").unwrap();
        let result = package_archive::validate_package_archive(saved.clone(), archive.hash.clone(), manifest.clone(), extraction.clone(), BTreeMap::new()).await;
        assert!(result.is_ok(), "{result:?}"); assert!(!extraction.join("stale").exists());
        let original = fixture.0.join("original-extract");
        compare("validatePackageArchive", vec![path(&saved), archive.hash.clone().into(), manifest.clone(), path(&original), IvyJson::object([] as [(&str, IvyJson);0])], result.map(|_| IvyJson::Null));
        let proof = helper(IvyJson::object([("mode", "compare".into()), ("source", path(&root)), ("first", path(&extraction.join("artifact"))), ("second", path(&original.join("artifact"))), ("archive", path(&saved))]));
        assert_eq!(proof.get("equal"), Some(&IvyJson::Bool(true))); record(if app {"ordinary-app"} else {"ordinary-service"}, proof);
        // Separate invocations can carry different tar/gzip timestamp metadata.
        // Verify each reported checksum against its own bytes and cross-open both.
        let original = helper(IvyJson::object([("mode", "originalCreate".into()), ("candidate", candidate)]));
        assert_eq!(original.get("ok"), Some(&IvyJson::Bool(true)), "{original:?}");
        let original = original.required("value").unwrap();
        assert_eq!(original.get("path"), Some(&path(&archive.path)));
        assert_eq!(original.required("bytes").unwrap().as_f64().unwrap(), fs::metadata(&archive.path).unwrap().len() as f64);
        assert_eq!(original.required("hash").unwrap().as_str().unwrap(), artifact::file_hash(archive.path.clone()).await.unwrap());
        package_archive::validate_package_archive(archive.path.clone(), original.required("hash").unwrap().as_str().unwrap(), manifest, extraction.clone(), BTreeMap::new()).await.unwrap();
        let proof = helper(IvyJson::object([("mode", "compare".into()), ("source", path(&root)), ("first", path(&extraction.join("artifact"))),
            ("second", path(&extraction.join("artifact"))), ("archive", path(&archive.path))]));
        assert_eq!(proof.get("equal"), Some(&IvyJson::Bool(true))); record("original-create-rust-receive", proof);
    }
}

#[tokio::test]
async fn archive_receive_refusals_match_original_before_and_after_extraction() {
    for mode in ["hash", "manifest", "stamp-build", "stamp-version", "stamp-schema", "empty-native", "missing-native", "directory-native", "missing-app-entry", "component-link", "artifact-link", "unexpected-root", "parent-path", "backslash", "duplicate", "payload-link", "entry-limit", "listing-limit"] {
        let (fixture, candidate, mut manifest) = fixture(if mode.ends_with("native") {"agent-manager"} else {"fixture"}, true).await;
        let root = PathBuf::from(candidate.required("artifactRoot").unwrap().as_str().unwrap());
        let package = root.parent().unwrap();
        if mode.ends_with("native") {
            fs::create_dir_all(root.join("dist/native")).unwrap();
            if mode == "directory-native" { fs::create_dir(root.join("dist/native/ivy-job.exe")).unwrap(); }
            else if mode == "empty-native" { fs::write(root.join("dist/native/ivy-job.exe"), b"").unwrap(); }
        }
        if mode.starts_with("stamp-") {
            let mut stamp = config::json_file(root.join("dist/build-info.json"), 1024).await.unwrap();
            match mode { "stamp-build" => stamp.insert("buildId", format!("sha256:{}", "c".repeat(64)).into()),
                "stamp-version" => stamp.insert("version", "2.0.0".into()), _ => stamp.insert("schemaVersion", IvyJson::Number(2.0)) };
            config::atomic_json(root.join("dist/build-info.json"), stamp).await.unwrap();
        }
        if mode == "missing-app-entry" { fs::remove_file(root.join("dist/apps/fixture/index.html")).unwrap(); }
        let archive = package_archive::create_package_archive(candidate.clone(), BTreeMap::new()).await.unwrap();
        if ["component-link", "artifact-link", "unexpected-root", "parent-path", "backslash", "duplicate", "payload-link", "entry-limit", "listing-limit"].contains(&mode) {
            helper(IvyJson::object([("mode", "mutate".into()), ("case", mode.into()), ("archive", path(&archive.path)), ("root", path(package))]));
        }
        let hash = if mode == "hash" { format!("sha256:{}", "0".repeat(64)) } else { artifact::file_hash(archive.path.clone()).await.unwrap() };
        if mode == "manifest" { manifest.insert("version", "2.0.0".into()); }
        let extraction = fixture.0.join("extract");
        let result = package_archive::validate_package_archive(archive.path.clone(), hash.clone(), manifest.clone(), extraction.clone(), BTreeMap::new()).await;
        if mode == "duplicate" { assert!(result.is_ok(), "old receiver permits duplicate regular entries: {result:?}"); }
        else { assert!(result.is_err(), "{mode}"); }
        record(mode, match &result {Ok(_) => IvyJson::Null, Err(error) => error.to_wire()});
        compare("validatePackageArchive", vec![path(&archive.path), hash.into(), manifest, path(&extraction), IvyJson::object([] as [(&str, IvyJson);0])], result.map(|_| IvyJson::Null));
    }
}

async fn native_fixture(unicode: bool) -> (ServiceFixture, IvyJson, IvyJson, Vec<PathBuf>) {
    let (fixture, mut candidate, manifest) = fixture("fixture", false).await;
    let old = fixture.0.join("package"); let package = fixture.0.join(if unicode {"native ü 空 😀"} else {"native package #="}); fs::rename(&old, &package).unwrap();
    let root = package.join("artifact"); candidate.insert("artifactRoot", path(&root)); candidate.insert("manifestPath", path(&package.join("component.json")));
    let staged = helper(IvyJson::object([("mode", "stage".into()), ("plan", manifest.clone()), ("root", path(&fixture.0.join("builder"))), ("outputRoot", path(&root.join("dist")))]));
    assert_eq!(staged.required("platforms").unwrap().as_array().unwrap().len(), 4);
    assert_eq!(staged.required("stamp").unwrap().get("version"), manifest.get("version"));
    assert_eq!(staged.required("identity").unwrap().get("version"), Some(&IvyJson::from("9.9.9")));
    fs::write(root.join(if unicode {"dist/read me #=ü 空 😀.txt"} else {"dist/read me #=.txt"}), b"\0\xff\xfe\n preserved bytes").unwrap();
    fs::write(root.join("dist").join(format!("{}-{}.txt", "long-".repeat(22), if unicode {"ü"} else {"name"})), b"long filename").unwrap();
    let readonly = root.join("dist/readonly.dat"); fs::write(&readonly, b"readonly payload").unwrap();
    let mut permissions = fs::metadata(&readonly).unwrap().permissions(); permissions.set_readonly(true); fs::set_permissions(readonly, permissions).unwrap();
    let files = staged.required("linuxExecutables").unwrap().as_array().unwrap().iter().map(|value| PathBuf::from(value.as_str().unwrap())).collect();
    record("shared-native-stager", staged);
    (fixture, candidate, manifest, files)
}

#[tokio::test]
async fn native_universal_archive_encodes_all_selected_linux_modes_and_cross_opens_original_receiver() {
    let (fixture, candidate, manifest, files) = native_fixture(false).await;
    let _cleanup = ReadonlyCleanup(fixture.0.clone());
    assert_eq!(files.len(), 6);
    let root = PathBuf::from(candidate.required("artifactRoot").unwrap().as_str().unwrap());
    let before: Vec<_> = files.iter().map(|file| fs::metadata(file).unwrap().permissions()).collect();
    let archive = package_archive::create_package_archive_with_linux_executables(candidate.clone(), files.clone()).await.unwrap();
    assert_eq!(archive.hash, artifact::file_hash(archive.path.clone()).await.unwrap());
    for (file, permissions) in files.iter().zip(before) { assert_eq!(fs::metadata(file).unwrap().permissions(), permissions); }
    let saved = fixture.0.join("native-universal.tar.gz"); fs::copy(&archive.path, &saved).unwrap();
    let rust = fixture.0.join("rust-receive"); let old = fixture.0.join("original-receive");
    let result = package_archive::validate_package_archive(saved.clone(), archive.hash.clone(), manifest.clone(), rust.clone(), BTreeMap::new()).await;
    assert!(result.is_ok(), "{result:?}");
    compare("validatePackageArchive", vec![path(&saved), archive.hash.into(), manifest.clone(), path(&old), IvyJson::object([] as [(&str, IvyJson);0])], result.map(|_| IvyJson::Null));
    let proof = helper(IvyJson::object([("mode", "compare".into()), ("source", path(&root)), ("first", path(&rust.join("artifact"))), ("second", path(&old.join("artifact"))), ("archive", path(&saved))]));
    let headers = proof.required("headers").unwrap().as_array().unwrap(); let mut names = std::collections::BTreeSet::new();
    for header in headers { assert!(names.insert(header.required("name").unwrap().as_str().unwrap()), "duplicate archive header"); }
    for file in &files {
        let name = format!("artifact/{}", file.strip_prefix(&root).unwrap().to_str().unwrap().replace('\\', "/"));
        let header = headers.iter().find(|header| header.get("name") == Some(&IvyJson::from(name.as_str()))).unwrap();
        assert_eq!(header.get("mode"), Some(&IvyJson::Number(0o755 as f64)));
    }
    #[cfg(windows)] {
        for header in headers.iter().filter(|header| header.get("type") == Some(&IvyJson::from("0"))) {
            let name = header.required("name").unwrap().as_str().unwrap();
            if !name.contains("/native/linux-") {
                assert_eq!(header.get("mode"), Some(&IvyJson::Number(if name.ends_with("readonly.dat") {0o444} else {0o666} as f64)), "{name}");
            }
        }
    }
    let mut received = candidate; received.insert("artifactRoot", path(&rust.join("artifact"))); received.insert("manifestPath", path(&rust.join("component.json")));
    assert_json_eq(&artifact::verify_candidate(received, rust, &super::fixture_node("24.18.0")).await.unwrap(), &manifest);
    record("native-modes-original-and-rust-receive", proof);
}

#[tokio::test]
async fn native_unicode_header_paths_are_exact_and_receiver_outcomes_match_original() {
    let (fixture, candidate, manifest, files) = native_fixture(true).await;
    let _cleanup = ReadonlyCleanup(fixture.0.clone());
    let archive = package_archive::create_package_archive_with_linux_executables(candidate, files).await.unwrap();
    let saved = fixture.0.join("unicode.tar.gz"); fs::copy(&archive.path, &saved).unwrap();
    let proof = helper(IvyJson::object([("mode", "headers".into()), ("archive", path(&saved))]));
    let headers = proof.required("headers").unwrap().as_array().unwrap();
    assert!(headers.iter().any(|h| h.get("name") == Some(&IvyJson::from("artifact/dist/read me #=ü 空 😀.txt"))));
    assert!(headers.iter().any(|h| h.get("name") == Some(&IvyJson::from(format!("artifact/dist/{}-ü.txt", "long-".repeat(22))))));
    record("native-unicode-standard-headers", proof);
    let rust = fixture.0.join("rust-unicode"); let old = fixture.0.join("original-unicode");
    let result = package_archive::validate_package_archive(saved.clone(), archive.hash.clone(), manifest.clone(), rust.clone(), BTreeMap::new()).await;
    // The unchanged Windows system tar can refuse UTF-8 PAX paths under its
    // current locale. Keep that original receiver limitation explicit; writing
    // valid Unicode metadata does not establish successful Windows extraction.
    compare("validatePackageArchive", vec![path(&saved), archive.hash.into(), manifest, path(&old), IvyJson::object([] as [(&str, IvyJson);0])], result.clone().map(|_| IvyJson::Null));
    if result.is_ok() {
        let proof = helper(IvyJson::object([("mode", "compare".into()), ("source", path(archive.path.parent().unwrap().join("artifact").as_path())),
            ("first", path(&rust.join("artifact"))), ("second", path(&old.join("artifact"))), ("archive", path(&saved))]));
        record("native-unicode-receivers", proof);
    } else {
        assert!(cfg!(windows), "Unicode archive receiver failed: {result:?}");
        assert_eq!(result.as_ref().unwrap_err().code, "command_failed");
        record("native-unicode-receiver-limitation", result.unwrap_err().to_wire());
    }
}

#[tokio::test]
async fn native_archive_rejects_invalid_explicit_modes_and_linked_inputs_before_writing() {
    let (fixture, candidate, _, files) = native_fixture(false).await;
    let _cleanup = ReadonlyCleanup(fixture.0.clone());
    let root = PathBuf::from(candidate.required("artifactRoot").unwrap().as_str().unwrap()); let destination = root.parent().unwrap().join("package.tar.gz");
    let outside = fixture.0.join("outside"); fs::write(&outside, b"retained outside bytes").unwrap();
    for (case, selected) in [("outside", vec![outside.clone()]), ("relative", vec![PathBuf::from("dist/native/linux-x64/ivy-fixture")]),
        ("windows", vec![root.join("dist/native/win32-x64/ivy-fixture.exe")]), ("directory", vec![root.join("dist/native/linux-x64")]),
        ("missing", vec![root.join("dist/native/linux-x64/missing")]), ("duplicate", vec![files[0].clone(), files[0].clone()])] {
        let result = package_archive::create_package_archive_with_linux_executables(candidate.clone(), selected).await;
        assert!(result.is_err(), "{case}"); assert!(!destination.exists()); record(case, result.unwrap_err().to_wire());
    }
    let selected = root.join("dist/native/linux-x64/empty"); fs::write(&selected, b"").unwrap();
    assert!(package_archive::create_package_archive_with_linux_executables(candidate.clone(), vec![selected]).await.is_err()); assert!(!destination.exists());
    let link = root.join("dist/link");
    #[cfg(windows)] std::os::windows::fs::symlink_file(&outside, &link).unwrap();
    #[cfg(unix)] std::os::unix::fs::symlink(&outside, &link).unwrap();
    let result = package_archive::create_package_archive_with_linux_executables(candidate, files).await;
    assert!(result.is_err()); assert!(!destination.exists()); assert_eq!(fs::read(outside).unwrap(), b"retained outside bytes"); record("native-linked-input", result.unwrap_err().to_wire());
}

#[tokio::test]
async fn native_archive_stops_at_the_existing_compressed_byte_limit() {
    let (_fixture, candidate, _, files) = native_fixture(false).await;
    let _cleanup = ReadonlyCleanup(_fixture.0.clone());
    let root = PathBuf::from(candidate.required("artifactRoot").unwrap().as_str().unwrap());
    helper(IvyJson::object([("mode", "large".into()), ("path", path(&root.join("dist/incompressible.bin")))]));
    let result = package_archive::create_package_archive_with_linux_executables(candidate, files).await;
    let error = result.unwrap_err(); assert_eq!(error.code, "artifact_changed", "{error:?}");
    let bytes = fs::metadata(root.parent().unwrap().join("package.tar.gz")).unwrap().len();
    assert!(bytes <= package_archive::MAXIMUM_ARCHIVE_BYTES);
    record("compressed-byte-limit", IvyJson::object([("error", error.to_wire()), ("partialBytes", IvyJson::Number(bytes as f64))]));
}
