use super::{ServiceFixture, compare_config_oracle};
use ivy_contracts::{IvyJson, IvyResult};
use ivy_host_runtime::{artifact, config, node_runtime::NodeRuntime, portable_native_files::portable_native_files};
use std::{fs, path::{Path, PathBuf}};

fn hash(c: &str) -> IvyJson { format!("sha256:{}", c.repeat(64)).into() }
fn path(value: &Path) -> IvyJson { value.to_string_lossy().to_string().into() }
fn compare(operation: &str, args: Vec<IvyJson>, result: IvyResult<IvyJson>) {
    compare_config_oracle(IvyJson::object([("kind", "artifact".into()), ("operation", operation.into()), ("args", IvyJson::Array(args)), ("node", "v24.18.0".into())]), result);
}
fn node() -> NodeRuntime { NodeRuntime { executable: PathBuf::from("unused"), version: "24.18.0".into() } }
pub(super) async fn fixture() -> (ServiceFixture, IvyJson, IvyJson) {
    let fixture = ServiceFixture::new(); let root = fixture.0.join("artifact");
    let candidate = IvyJson::object([("candidateId", hash("a")), ("componentId", "fixture".into()), ("artifactRoot", path(&root)), ("manifestPath", path(&root.join("manifest.json"))), ("createdAt", "2026-01-01T00:00:00.000Z".into()),
        ("platform", IvyJson::object([("os", "win32".into()), ("arch", "x64".into()), ("node", "24.18.0".into())])), ("buildId", hash("a"))]);
    let manifest = IvyJson::object([("schemaVersion", IvyJson::Number(1.0)), ("componentId", "fixture".into()), ("kind", "app".into()), ("version", "1.0.0".into()), ("buildId", hash("a")), ("connectsToHive", false.into()),
        ("requirements", IvyJson::object([("node", ">=24.18.0 <25.0.0".into()), ("hiveProtocol", IvyJson::Null), ("contracts", IvyJson::Array(vec![]))])),
        ("app", IvyJson::object([("appId", "fixture".into()), ("dist", "dist/apps/fixture".into()), ("entryPath", "index.html".into())]))]);
    config::atomic_json(root.join("manifest.json"), manifest.clone()).await.unwrap();
    config::atomic_json(root.join("dist/build-info.json"), IvyJson::object([("buildId", hash("a"))])).await.unwrap();
    fs::write(root.join("dist/main.js"), b"fixture").unwrap();
    (fixture, candidate, manifest)
}
fn host(root: &Path) -> IvyJson { IvyJson::object([("artifactRoot", path(root))]) }
async fn verify(fixture: &ServiceFixture, candidate: &IvyJson) -> IvyResult<IvyJson> {
    artifact::verify_candidate(candidate.clone(), fixture.0.join("artifact"), &node()).await
}

#[tokio::test]
async fn archive_hash_checks_regular_files_exact_checksum_and_changed_bytes() {
    let fixture = ServiceFixture::new(); let source = fixture.0.join("archive.tgz");
    // Cross a 4 MiB chunk boundary; hash the bytes, not text or an archive listing.
    let bytes: Vec<u8> = (0..(4 * 1024 * 1024 + 17)).map(|i| (i % 251) as u8).collect(); fs::write(&source, bytes).unwrap();
    let digest = artifact::file_hash(source.clone()).await.unwrap();
    compare("fileHash", vec![path(&source)], Ok(digest.clone().into()));
    for expected in [digest.clone(), digest.to_uppercase(), "sha256:bad".into(), format!("sha256:{}", "0".repeat(64))] {
        compare("verifyTransferredArchive", vec![path(&source), expected.clone().into()], artifact::verify_transferred_archive(source.clone(), &expected).await.map(|_| IvyJson::Null));
    }
    fs::write(&source, b"replaced").unwrap();
    compare("verifyTransferredArchive", vec![path(&source), digest.clone().into()], artifact::verify_transferred_archive(source, &digest).await.map(|_| IvyJson::Null));
    compare("verifyTransferredArchive", vec![path(&fixture.0), digest.clone().into()], artifact::verify_transferred_archive(fixture.0.clone(), &digest).await.map(|_| IvyJson::Null));
}

#[tokio::test]
async fn launch_checks_containment_selected_entry_and_build_without_rehashing_release() {
    let (fixture, candidate, _) = fixture().await; let root = fixture.0.join("artifact");
    compare_config_oracle(IvyJson::object([("kind", "realpath".into()), ("path", path(&root))]), Ok(path(&ivy_host_runtime::paths::realpath(&root).unwrap())));
    for entry in [None, Some("dist/main.js"), Some("outside-selected-entry.js")] {
        compare("verifyLaunchCandidate", vec![candidate.clone(), host(&root), entry.map(IvyJson::from).unwrap_or(IvyJson::Null)], artifact::verify_launch_candidate(candidate.clone(), root.clone(), entry.map(ivy_contracts::JsString::from)).await.map(|_| IvyJson::Null));
    }
    fs::write(root.join("unrelated.bin"), b"not rehashed").unwrap();
    artifact::verify_launch_candidate(candidate.clone(), root.clone(), None).await.unwrap();
    fs::remove_file(root.join("dist/main.js")).unwrap(); fs::create_dir(root.join("dist/main.js")).unwrap();
    compare("verifyLaunchCandidate", vec![candidate.clone(), host(&root), "dist/main.js".into()], artifact::verify_launch_candidate(candidate.clone(), root.clone(), Some("dist/main.js".into())).await.map(|_| IvyJson::Null));
    compare("verifyLaunchCandidate", vec![candidate.clone(), host(&fixture.0.join("other")), IvyJson::Null], artifact::verify_launch_candidate(candidate.clone(), fixture.0.join("other"), None).await.map(|_| IvyJson::Null));
    config::atomic_json(root.join("dist/build-info.json"), IvyJson::object([("buildId", hash("b"))])).await.unwrap();
    compare("verifyLaunchCandidate", vec![candidate.clone(), host(&root), IvyJson::Null], artifact::verify_launch_candidate(candidate, root, None).await.map(|_| IvyJson::Null));
}

#[tokio::test]
async fn candidate_checks_manifest_identity_runtime_and_required_native_payloads() {
    let (fixture, mut candidate, mut manifest) = fixture().await; let root = fixture.0.join("artifact");
    compare("verifyCandidate", vec![candidate.clone(), host(&root)], verify(&fixture, &candidate).await);
    manifest.insert("buildId", hash("b")); config::atomic_json(root.join("manifest.json"), manifest.clone()).await.unwrap();
    compare("verifyCandidate", vec![candidate.clone(), host(&root)], verify(&fixture, &candidate).await);
    manifest.insert("buildId", hash("a")); manifest.insert("kind", "native".into()); manifest.remove("app");
    manifest.insert("entrypoint", IvyJson::object([("executable", "$node".into()), ("args", IvyJson::Array(vec!["dist/main.js".into()])), ("timeoutMs", IvyJson::Number(10_000.0))]));
    manifest.insert("readiness", IvyJson::object([("timeoutMs", IvyJson::Number(10_000.0)), ("command", manifest.required("entrypoint").unwrap().clone())]));
    manifest.insert("shutdown", IvyJson::object([("timeoutMs", IvyJson::Number(1000.0))]));
    manifest.insert("restart", IvyJson::object([("policy", "never".into()), ("minimumDelayMs", IvyJson::Number(100.0)), ("maximumDelayMs", IvyJson::Number(100.0))]));
    let mut requirements = manifest.required("requirements").unwrap().clone(); requirements.insert("node", ">=25.0.0 <26.0.0".into()); manifest.insert("requirements", requirements.clone());
    config::atomic_json(root.join("manifest.json"), manifest.clone()).await.unwrap();
    let result = verify(&fixture, &candidate).await; assert_eq!(result.as_ref().unwrap_err().code, "unsupported_runtime");
    compare("verifyCandidate", vec![candidate.clone(), host(&root)], result);
    requirements.insert("node", ">=24.18.0 <25.0.0".into()); requirements.insert("arch", IvyJson::Array(vec![if cfg!(target_arch="x86_64") { "arm64".into() } else { "x64".into() }])); manifest.insert("requirements", requirements.clone());
    config::atomic_json(root.join("manifest.json"), manifest.clone()).await.unwrap();
    compare("verifyCandidate", vec![candidate.clone(), host(&root)], verify(&fixture, &candidate).await);
    requirements.remove("arch"); manifest.insert("requirements", requirements); manifest.insert("componentId", "host-executor".into()); candidate.insert("componentId", "host-executor".into());
    config::atomic_json(root.join("manifest.json"), manifest.clone()).await.unwrap();
    compare("verifyCandidate", vec![candidate.clone(), host(&root)], verify(&fixture, &candidate).await);
    for file in portable_native_files("host-executor") { let file=root.join(file); fs::create_dir_all(file.parent().unwrap()).unwrap(); fs::write(file,b"payload").unwrap(); }
    super::assert_json_eq(&verify(&fixture, &candidate).await.unwrap(), &manifest);
    compare("verifyCandidate", vec![candidate.clone(), host(&root)], verify(&fixture, &candidate).await);
    for component in ["host-executor", "service-manager", "agent-manager", "phone-bridge", "unknown"] {
        compare_config_oracle(IvyJson::object([("kind", "portableNativeFiles".into()), ("componentId", component.into())]), Ok(IvyJson::Array(portable_native_files(component).iter().map(|v| IvyJson::from(*v)).collect())));
    }
}

#[tokio::test]
async fn candidate_rejects_linked_launch_stamp_archive_and_escaping_manifest() {
    let (fixture, candidate, mut manifest) = fixture().await; let root=fixture.0.join("artifact");
    let outside=fixture.0.join("outside.json"); config::atomic_json(outside.clone(), IvyJson::object([("buildId", hash("a"))])).await.unwrap();
    let stamp=root.join("dist/build-info.json"); fs::remove_file(&stamp).unwrap();
    #[cfg(windows)] std::os::windows::fs::symlink_file(&outside,&stamp).unwrap();
    #[cfg(unix)] std::os::unix::fs::symlink(&outside,&stamp).unwrap();
    compare("verifyLaunchCandidate", vec![candidate.clone(),host(&root),IvyJson::Null],artifact::verify_launch_candidate(candidate.clone(),root.clone(),None).await.map(|_|IvyJson::Null));
    let digest=artifact::file_hash(outside.clone()).await.unwrap();
    compare("verifyTransferredArchive",vec![path(&stamp),digest.clone().into()],artifact::verify_transferred_archive(stamp.clone(),&digest).await.map(|_|IvyJson::Null));
    fs::remove_file(stamp).unwrap();
    config::atomic_json(root.join("dist/build-info.json"),IvyJson::object([("buildId",hash("a"))])).await.unwrap();
    manifest.insert("legacyProvenance", "ignored".into()); config::atomic_json(outside.clone(),manifest).await.unwrap();
    fs::remove_file(root.join("manifest.json")).unwrap();
    #[cfg(windows)] std::os::windows::fs::symlink_file(&outside,root.join("manifest.json")).unwrap();
    #[cfg(unix)] std::os::unix::fs::symlink(&outside,root.join("manifest.json")).unwrap();
    compare("verifyCandidate",vec![candidate.clone(),host(&root)],verify(&fixture,&candidate).await);
}
