use super::*;
use ivy_host_runtime::{filesystem, native_path};

async fn serve(assets: &Assets, method: &str, path: &str, reply: &mut Reply) -> IvyResult<bool> {
    let method = JsString::from(method);
    let relative = JsString::from(path);
    let base = JsString::from("/ivy");
    if assets
        .public(Some(&method), &relative, &base, reply)
        .await?
    {
        return Ok(true);
    }
    assets
        .console(
            Some(&method),
            &format!("/ivy{path}").into(),
            &relative,
            &base,
            reply,
        )
        .await
}
fn response_bytes(reply: &Reply) -> Vec<u8> {
    let event = reply
        .events
        .iter()
        .find(|event| event.get("kind") == Some(&"bytes".into()))
        .unwrap();
    STANDARD
        .decode(event.required("base64").unwrap().as_str().unwrap())
        .unwrap()
}
fn response_head(reply: &Reply, status: u16, mime: Option<&str>) {
    let event = reply
        .events
        .iter()
        .find(|event| event.get("kind") == Some(&"head".into()))
        .unwrap();
    assert_eq!(
        event.get("status"),
        Some(&IvyJson::Number(f64::from(status)))
    );
    if let Some(mime) = mime {
        assert_eq!(
            event.required("headers").unwrap().get("Content-Type"),
            Some(&mime.into())
        );
    }
}

#[tokio::test]
async fn native_browser_assets_and_authenticated_console_files() {
    let root = Root::new();
    for unit in [0xd800, 0xdfff] {
        #[cfg(windows)]
        let name = {
            use std::{ffi::OsString, os::windows::ffi::OsStringExt};
            OsString::from_wide(
                &format!("console-é-{unit:x}-")
                    .encode_utf16()
                    .chain([unit])
                    .collect::<Vec<_>>(),
            )
        };
        #[cfg(not(windows))]
        let name = std::ffi::OsString::from(format!("console-é-{unit:x}"));
        let console = fs::canonicalize(&root.0).unwrap().join(&name);
        let alias = root.0.join(name.to_string_lossy().as_ref());
        if alias != console && cfg!(windows) {
            fs::create_dir(&alias).unwrap();
            fs::write(alias.join("sentinel"), b"preserved alias").unwrap();
        }
        fs::create_dir_all(console.join("icons")).unwrap();
        fs::create_dir(console.join("directory")).unwrap();
        let files: [(&str, &[u8], &str, &str); 8] = [
            ("index.html", b"<html>fixture</html>", "/", "text/html"),
            (
                "app.js",
                b"const fixture=1;",
                "/console/app.js",
                "text/javascript",
            ),
            ("app.css", b"body{}", "/console/app.css", "text/css"),
            ("font.woff2", b"font", "/console/font.woff2", "font/woff2"),
            (
                "upper.JS",
                b"upper",
                "/console/upper.JS",
                "application/octet-stream",
            ),
            (
                "a+b.txt",
                b"plus",
                "/console/a+b.txt",
                "application/octet-stream",
            ),
            (
                "é.txt",
                b"accent",
                "/console/%C3%A9.txt",
                "application/octet-stream",
            ),
            (
                "file space",
                b"space",
                "/console/file%20space",
                "application/octet-stream",
            ),
        ];
        for (name, bytes, _, _) in files {
            fs::write(console.join(name), bytes).unwrap();
        }
        fs::File::create(console.join("too-large.bin"))
            .unwrap()
            .set_len(32 * 1024 * 1024 + 1)
            .unwrap();
        let outside = root.0.join(format!("outside-{unit:x}"));
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret"), b"outside").unwrap();
        filesystem::directory_link(&outside, &console.join("escape")).unwrap();
        let assets = Assets {
            console_root: Some(console.clone()),
        };
        for (_, bytes, path, mime) in files {
            let mut reply = Reply::default();
            assert!(serve(&assets, "GET", path, &mut reply).await.unwrap());
            response_head(&reply, 200, Some(mime));
            assert_eq!(response_bytes(&reply), bytes);
            assert!(reply.ended);
        }
        for size in [180, 192, 512] {
            let bytes = [0, 255, 13, 10, size as u8];
            fs::write(console.join(format!("icons/icon-{size}.png")), bytes).unwrap();
            let mut reply = Reply::default();
            assert!(
                serve(&assets, "GET", &format!("/app/icon-{size}.png"), &mut reply)
                    .await
                    .unwrap()
            );
            response_head(&reply, 200, Some("image/png"));
            assert_eq!(response_bytes(&reply), bytes);
        }
        for base in ["", "/ivy", "/é"] {
            let manifest = ivy_hive::http_static::manifest(&base.into());
            for field in ["id", "start_url", "scope"] {
                assert_eq!(manifest.get(field), Some(&format!("{base}/").into()));
            }
        }
        let opaque = JsString(vec![47, 0xd800]);
        let manifest = ivy_hive::http_static::manifest(&opaque);
        assert_eq!(
            manifest.get("scope"),
            Some(&IvyJson::String(JsString(vec![47, 0xd800, 47])))
        );
        let mut reply = Reply::default();
        assert!(
            serve(&assets, "GET", "/app/manifest.webmanifest", &mut reply)
                .await
                .unwrap()
        );
        response_head(&reply, 200, Some("application/manifest+json"));
        let manifest = parse(
            &String::from_utf8(response_bytes(&reply)).unwrap(),
            usize::MAX,
        )
        .unwrap();
        assert_eq!(manifest.get("scope"), Some(&"/ivy/".into()));
        let mut reply = Reply::default();
        assert!(
            serve(&assets, "GET", "/app-worker.js", &mut reply)
                .await
                .unwrap()
        );
        assert_eq!(response_bytes(&reply), ivy_hive::http_static::WORKER);
        for path in ["/mcp-icon.svg", "/mcp-dev-icon.svg"] {
            let mut reply = Reply::default();
            assert!(serve(&assets, "GET", path, &mut reply).await.unwrap());
            assert!(!response_bytes(&reply).is_empty());
        }
        for path in ["/console/index.html"] {
            let mut reply = Reply::default();
            assert!(serve(&assets, "GET", path, &mut reply).await.unwrap());
            response_head(&reply, 303, None);
            assert_eq!(
                reply.events[0].required("headers").unwrap().get("Location"),
                Some(&"/ivy/".into())
            );
        }
        for (path, code) in [
            ("/console/directory", "not_found"),
            ("/console/absent", "not_found"),
            ("/console/too-large.bin", "not_found"),
            ("/console/escape/secret", "not_found"),
            ("/console/%252e%252e", "invalid_arguments"),
        ] {
            let mut reply = Reply::default();
            assert_eq!(
                serve(&assets, "GET", path, &mut reply)
                    .await
                    .unwrap_err()
                    .code,
                code,
                "{path}"
            );
            assert!(reply.events.is_empty());
        }
        for path in [
            "/console/%2e%2e%2foutside.txt",
            "/console/%5csecret",
            "/console/%00",
            "/console/%E9",
            "/console/%EF%BB%BFindex.html",
            "/console/e%CC%81.txt",
        ] {
            let mut reply = Reply::default();
            assert!(serve(&assets, "GET", path, &mut reply).await.is_err());
            assert!(reply.events.is_empty());
        }
        for method in ["POST", "HEAD"] {
            for path in [
                "/",
                "/console/app.js",
                "/app-worker.js",
                "/app/icon-192.png",
            ] {
                let mut reply = Reply::default();
                assert!(!serve(&assets, method, path, &mut reply).await.unwrap());
                assert!(reply.events.is_empty());
            }
        }
        for path in ["", "/app/icon-42.png"] {
            assert!(
                !serve(&assets, "GET", path, &mut Reply::default())
                    .await
                    .unwrap()
            );
        }
        let mut reply = Reply {
            throw_head: true,
            ..Reply::default()
        };
        assert!(
            serve(&assets, "GET", "/app-worker.js", &mut reply)
                .await
                .is_err()
        );
        assert_eq!(reply.events.len(), 1);
        assert!(!reply.ended);
        let mut reply = Reply {
            throw_header: "Cache-Control".into(),
            ..Reply::default()
        };
        assert!(
            serve(&assets, "GET", "/mcp-icon.svg", &mut reply)
                .await
                .is_err()
        );
        assert_eq!(reply.events.len(), 1);
        assert!(!reply.ended);
        assert_eq!(fs::read(outside.join("secret")).unwrap(), b"outside");
        assert!(native_path::from_native(&console).0.contains(&unit) || !cfg!(windows));
        if cfg!(windows) {
            assert_eq!(
                fs::read(alias.join("sentinel")).unwrap(),
                b"preserved alias"
            );
        }
    }
    for root in [None, Some(PathBuf::new())] {
        let assets = Assets { console_root: root };
        assert_eq!(
            serve(&assets, "GET", "/", &mut Reply::default())
                .await
                .unwrap_err()
                .code,
            "service_unavailable"
        );
        assert_eq!(
            serve(&assets, "GET", "/app/icon-192.png", &mut Reply::default())
                .await
                .unwrap_err()
                .code,
            "not_found"
        );
        assert!(
            serve(&assets, "GET", "/app-worker.js", &mut Reply::default())
                .await
                .unwrap()
        );
    }
}
