use super::*;

struct Server {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn server(archive: Vec<u8>) -> Server {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/prefix%20space/?old=yes#old",
        listener.local_addr().unwrap()
    );
    let requests = Arc::new(Mutex::new(vec![]));
    let seen = requests.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = vec![];
            loop {
                let mut chunk = [0; 1024];
                let count = socket.read(&mut chunk).await.unwrap();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..count]);
                if bytes.windows(4).any(|v| v == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(bytes).unwrap();
            seen.lock().unwrap().push(request.clone());
            let (status, body) = if request
                .starts_with("GET /prefix%20space/api/v1/packages/domain/1.0.0/artifact ")
            {
                ("200 OK", archive.clone())
            } else if request.contains("/missing/") {
                ("404 Not Found", vec![])
            } else if request.contains("/reset/") {
                ("205 Reset Content", vec![])
            } else if request.contains("/unavailable/") {
                ("503 Service Unavailable", vec![])
            } else {
                ("204 No Content", vec![])
            };
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            if socket.write_all(header.as_bytes()).await.is_err() {
                continue;
            }
            for chunk in body.chunks(4096) {
                if socket.write_all(chunk).await.is_err() {
                    break;
                }
            }
            let _ = socket.shutdown().await;
        }
    });
    Server {
        base,
        requests,
        task,
    }
}
fn client() -> NativePackageClient {
    NativePackageClient(reqwest::Client::builder().no_proxy().build().unwrap())
}
fn image(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut result = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(path) = pending.pop() {
        let info = fs::symlink_metadata(&path).unwrap();
        assert!(!info.file_type().is_symlink());
        let value = if info.is_file() {
            Some(fs::read(&path).unwrap())
        } else {
            pending.extend(fs::read_dir(&path).unwrap().map(|v| v.unwrap().path()));
            None
        };
        result.insert(path.strip_prefix(root).unwrap().to_owned(), value);
    }
    result
}
fn assert_requests(server: &Server) {
    for request in server.requests.lock().unwrap().iter() {
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer fixture-credential\r\n")
        );
        assert!(request.starts_with("GET /prefix%20space/api/v1/packages/"));
        assert!(!request.lines().next().unwrap().contains("old=yes"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn package_import_streams_real_archive_validates_and_replays_registered_and_unregistered_current_directories()
 {
    let mut f = Fixture::new(vec![instance("domain", "domain", true)]).await;
    let pair = f.release("domain", "1.0.0", "download-domain").await;
    let candidate = pair.required("candidate").unwrap();
    let source = path(candidate.required("manifestPath").unwrap())
        .parent()
        .unwrap()
        .to_owned();
    let before = image(&source);
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(encoder);
    tar.append_path_with_name(source.join("component.json"), "component.json")
        .unwrap();
    tar.append_dir_all("artifact", source.join("artifact"))
        .unwrap();
    let archive = tar.into_inner().unwrap().finish().unwrap();
    let mut entry = entry(&pair, 1.0);
    entry.insert("archiveHash", ivy_contracts::hash::digest(&archive).into());
    entry.insert("bytes", J::Number(archive.len() as f64));
    let build = id(candidate, "candidateId");
    assert!(f.journal.retire_candidate(&build).await.unwrap());
    f.journal.finish_candidate_retirement(&build).await.unwrap();
    let server = server(archive.clone()).await;
    f.config.insert("publicBaseUrl", server.base.clone().into());
    f.journal.use_configuration(f.config.clone()).await.unwrap();
    let destination = path(f.config.required("artifactRoot").unwrap())
        .join("candidates")
        .join(&build[7..]);
    let client = Arc::new(client());
    let updater = PackageUpdater::new(
        f.journal.clone(),
        "fixture-credential".into(),
        node(),
        client.clone(),
    );
    let imported = updater.import_package(entry.clone()).await.unwrap();
    let expected = J::object([
        ("candidateId", build.clone().into()),
        ("componentId", "domain".into()),
        ("buildId", build.clone().into()),
        ("artifactRoot", p(&destination.join("artifact"))),
        ("manifestPath", p(&destination.join("component.json"))),
        ("archivePath", p(&destination.join("package.tar.gz"))),
        (
            "archiveHash",
            entry.required("archiveHash").unwrap().clone(),
        ),
        ("createdAt", DATE.into()),
        ("platform", candidate.required("platform").unwrap().clone()),
    ]);
    assert_json_eq(&imported, &expected);
    assert_json_eq(&f.journal.candidate(&build).await.unwrap(), &expected);
    assert_json_eq(
        &f.journal.manifest(&build).await.unwrap(),
        pair.required("manifest").unwrap(),
    );
    assert_eq!(
        image(&destination.join("artifact")),
        image(&source.join("artifact"))
    );
    assert_eq!(
        fs::read(destination.join("component.json")).unwrap(),
        fs::read(source.join("component.json")).unwrap()
    );
    assert_eq!(
        fs::read(destination.join("package.tar.gz")).unwrap(),
        archive
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    assert_json_eq(
        &updater.import_package(entry.clone()).await.unwrap(),
        &expected,
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    assert!(f.journal.retire_candidate(&build).await.unwrap());
    f.journal.finish_candidate_retirement(&build).await.unwrap();
    assert_json_eq(
        &updater.import_package(entry.clone()).await.unwrap(),
        &expected,
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    let mut wrong = entry.clone();
    wrong.insert("archiveHash", hash("changed-archive"));
    assert_eq!(
        updater.import_package(wrong).await.unwrap_err().code,
        "artifact_changed"
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    assert!(
        client
            .catalog(server.base.clone(), "fixture-credential".into(), 7.0)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .starts_with("GET /prefix%20space/api/v1/packages/catalog?after=7 ")
    );
    assert!(
        !path(f.config.required("stagingRoot").unwrap())
            .join(format!(
                "package-{}-extract",
                &id(&entry, "archiveHash")[7..]
            ))
            .exists()
    );
    assert!(
        !path(f.config.required("artifactRoot").unwrap())
            .join(format!(
                "incoming-package-{}",
                &id(&entry, "archiveHash")[7..]
            ))
            .exists()
    );
    let mut missing = entry.clone();
    missing.insert("componentId", "missing".into());
    let missing_path = f.root.0.join("must-not-exist");
    assert_eq!(
        client
            .download(
                server.base.clone(),
                "fixture-credential".into(),
                missing,
                missing_path.clone()
            )
            .await
            .unwrap_err()
            .code,
        "not_found"
    );
    assert!(!missing_path.exists());
    assert!(f.journal.retire_candidate(&build).await.unwrap());
    f.journal.finish_candidate_retirement(&build).await.unwrap();
    fs::remove_dir_all(&destination).unwrap();
    for failure in ["changed-archive", "stale-download"] {
        let mut selected = entry.clone();
        if failure == "changed-archive" {
            selected.insert("archiveHash", hash("unexpected-download"));
        }
        let temporary = path(f.config.required("stagingRoot").unwrap()).join(format!(
            "package-{}.tar.gz",
            &id(&selected, "archiveHash")[7..]
        ));
        if failure == "stale-download" {
            fs::write(&temporary, b"retained interrupted download").unwrap();
        }
        let requests = server.requests.lock().unwrap().len();
        let error = updater.import_package(selected).await.unwrap_err();
        assert_eq!(
            error.code,
            if failure == "changed-archive" {
                "artifact_changed"
            } else {
                "internal_error"
            }
        );
        assert_eq!(server.requests.lock().unwrap().len(), requests + 1);
        assert!(!temporary.exists());
        assert!(!destination.exists());
        assert_eq!(
            f.journal.candidate(&build).await.unwrap_err().code,
            "not_found"
        );
    }
    assert_eq!(image(&source), before);
    assert_requests(&server);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn package_download_keeps_native_paths_exclusive_ownership_and_status_order() {
    let payload: Vec<u8> = (0..131_079).map(|n| (n % 251) as u8).collect();
    let server = server(payload.clone()).await;
    let client = client();
    let entry = J::object([
        ("componentId", "domain".into()),
        ("version", "1.0.0".into()),
    ]);
    let mut cases = vec!["plain", "unicode-dot", "canonical", "linked"];
    #[cfg(windows)]
    cases.extend(["opaque-high", "opaque-low", "trailing"]);
    for case in cases {
        let f = ServiceFixture::new();
        let root = f.0.join("downloads");
        fs::create_dir(&root).unwrap();
        let canonical = fs::canonicalize(&root).unwrap();
        let mut archive = root.join("archive.tar.gz");
        let mut alias = None;
        if case == "unicode-dot" {
            fs::create_dir(root.join("目录 space")).unwrap();
            archive = root.join("目录 space").join(".").join("archive.tar.gz");
        } else if case == "canonical" {
            archive = canonical.join("archive.tar.gz");
        } else if case == "linked" {
            fs::create_dir(root.join("real")).unwrap();
            ivy_host_runtime::filesystem::directory_link(Path::new("real"), &root.join("link"))
                .unwrap();
            archive = root.join("link").join("archive.tar.gz");
        }
        #[cfg(windows)]
        if case.starts_with("opaque") || case == "trailing" {
            use std::{ffi::OsString, os::windows::ffi::OsStringExt};
            if case == "trailing" {
                archive = canonical.join("archive.tar.gz. ");
                alias = Some(canonical.join("archive.tar.gz"));
            } else {
                let mut name: Vec<u16> = "archive".encode_utf16().collect();
                name.push(if case == "opaque-high" {
                    0xd800
                } else {
                    0xdfff
                });
                name.extend(".tar.gz".encode_utf16());
                archive = canonical.join(OsString::from_wide(&name));
                alias = Some(canonical.join("archive�.tar.gz"));
            }
        }
        if let Some(alias) = &alias {
            fs::write(alias, b"unrelated native alias").unwrap();
        }
        let before = server.requests.lock().unwrap().len();
        client
            .download(
                server.base.clone(),
                "fixture-credential".into(),
                entry.clone(),
                archive.clone(),
            )
            .await
            .unwrap();
        assert_eq!(fs::read(&archive).unwrap(), payload, "{case}");
        assert_eq!(server.requests.lock().unwrap().len(), before + 1, "{case}");
        let hardlink = canonical.join("owned-hardlink.tar.gz");
        fs::hard_link(&archive, &hardlink).unwrap();
        assert_eq!(
            client
                .download(
                    server.base.clone(),
                    "fixture-credential".into(),
                    entry.clone(),
                    archive.clone()
                )
                .await
                .unwrap_err()
                .code,
            "internal_error",
            "{case}"
        );
        assert_eq!(fs::read(&archive).unwrap(), payload, "{case}");
        assert_eq!(fs::read(&hardlink).unwrap(), payload, "{case}");
        assert_eq!(server.requests.lock().unwrap().len(), before + 2, "{case}");
        if let Some(alias) = &alias {
            assert_eq!(
                fs::read(alias).unwrap(),
                b"unrelated native alias",
                "{case}"
            );
        }
    }
    let f = ServiceFixture::new();
    for (component, expected) in [
        ("missing", "not_found"),
        ("empty", "service_unavailable"),
        ("reset", "service_unavailable"),
        ("unavailable", "service_unavailable"),
    ] {
        let target = f.0.join(component);
        fs::write(&target, b"retained destination").unwrap();
        let mut selected = entry.clone();
        selected.insert("componentId", component.into());
        assert_eq!(
            client
                .download(
                    server.base.clone(),
                    "fixture-credential".into(),
                    selected,
                    target.clone()
                )
                .await
                .unwrap_err()
                .code,
            expected
        );
        assert_eq!(fs::read(&target).unwrap(), b"retained destination");
    }
    let directory = f.0.join("directory");
    fs::create_dir(&directory).unwrap();
    let absent = f.0.join("missing-parent/archive.tar.gz");
    for target in [directory.clone(), absent.clone()] {
        assert_eq!(
            client
                .download(
                    server.base.clone(),
                    "fixture-credential".into(),
                    entry.clone(),
                    target
                )
                .await
                .unwrap_err()
                .code,
            "internal_error"
        );
    }
    assert!(directory.is_dir());
    assert!(!absent.parent().unwrap().exists());
    assert_requests(&server);
}
