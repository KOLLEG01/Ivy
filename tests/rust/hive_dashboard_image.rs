use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{IvyError, IvyJson as J, IvyResult, JsString};
use ivy_hive::{dashboard_image, http_response::Response, http_static::AssetResponse};
use std::{cell::Cell, collections::BTreeMap};

#[derive(Default)]
struct Reply {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
    ended: bool,
}
impl Response for Reply {
    fn writable_ended(&self) -> bool {
        self.ended
    }
    fn destroyed(&self) -> bool {
        false
    }
    fn headers_sent(&self) -> bool {
        self.status != 0
    }
    fn set_header(&mut self, name: &str, value: &JsString) -> IvyResult<()> {
        self.headers.insert(name.into(), value.to_utf8()?);
        Ok(())
    }
    fn write_head(&mut self, status: u16, headers: J) -> IvyResult<()> {
        self.status = status;
        for (name, value) in headers.as_object().unwrap() {
            self.set_header(&name.to_utf8()?, value.as_js_string().unwrap())?;
        }
        Ok(())
    }
    fn end(&mut self, value: String) -> IvyResult<()> {
        self.end_bytes(value.as_bytes())
    }
    fn end_empty(&mut self) -> IvyResult<()> {
        self.ended = true;
        Ok(())
    }
    fn destroy(&mut self) -> IvyResult<()> {
        Ok(())
    }
}
impl AssetResponse for Reply {
    fn end_bytes(&mut self, bytes: &[u8]) -> IvyResult<()> {
        self.body.extend_from_slice(bytes);
        self.ended = true;
        Ok(())
    }
}
fn image() -> J {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.resize(33, 0);
    J::object([
        ("mimeType", "image/png".into()),
        ("encoding", "base64".into()),
        ("data", STANDARD.encode(png).into()),
        (
            "metadata",
            J::object([
                ("display", "Native ä".into()),
                ("refreshSeconds", J::Number(60.0)),
            ]),
        ),
    ])
}
const QUERY: &str =
    "node=reference.dashboards&id=dashboard&token=secret&width=120&height=100&colorMode=grayscale";

#[tokio::test]
async fn public_png_keeps_headers_and_conditional_head_behavior() {
    let mut etag = String::new();
    for (method, conditional, status) in [
        ("GET", false, 200),
        ("HEAD", false, 200),
        ("GET", true, 304),
    ] {
        let mut reply = Reply::default();
        let matching = format!("\"other\", {etag}");
        dashboard_image::handle(
            method,
            &QUERY.into(),
            conditional.then_some(matching.as_str()),
            |node, args| async move {
                assert_eq!(node, JsString::from("reference.dashboards"));
                assert_eq!(args.get("id"), Some(&"dashboard".into()));
                assert_eq!(args.get("width"), Some(&J::Number(120.0)));
                assert_eq!(args.get("colorMode"), Some(&"grayscale".into()));
                Ok(image())
            },
            &mut reply,
        )
        .await
        .unwrap();
        assert_eq!(reply.status, status);
        assert_eq!(reply.headers["Access-Control-Allow-Origin"], "*");
        assert_eq!(reply.headers["Cache-Control"], "private, no-cache");
        assert_eq!(reply.headers["X-Dashboard-Refresh-Seconds"], "60");
        assert!(reply.headers["X-Dashboard-Metadata"].contains("Native%20%C3%A4"));
        if etag.is_empty() {
            etag = reply.headers["ETag"].clone();
        }
        assert_eq!(reply.headers["ETag"], etag);
        assert_eq!(
            reply.body.len(),
            if method == "GET" && status == 200 {
                33
            } else {
                0
            }
        );
    }
}

#[tokio::test]
async fn invalid_queries_and_methods_never_dispatch_a_provider_call() {
    for (method, query, expected) in [
        ("POST", QUERY, 405),
        ("GET", "", 400),
        ("GET", "node=n&id=i", 401),
        ("HEAD", "node=n&id=i", 401),
        ("GET", "node=n&id=i&token=t&node=other", 400),
        ("GET", "node=n&id=i&token=t&extra=x", 400),
        ("GET", "node=n&id=i&token=t&width=-1", 400),
        ("GET", "node=n&id=i&token=t&width=", 400),
        (
            "GET",
            "node=n&id=i&token=t&height=999999999999999999999",
            400,
        ),
    ] {
        let calls = Cell::new(0);
        let mut reply = Reply::default();
        dashboard_image::handle(
            method,
            &query.into(),
            None,
            |_, _| {
                calls.set(calls.get() + 1);
                std::future::ready(Ok(image()))
            },
            &mut reply,
        )
        .await
        .unwrap();
        assert_eq!(reply.status, expected, "{method} {query}");
        assert_eq!(calls.get(), 0);
        if method == "HEAD" {
            assert!(reply.body.is_empty());
        }
    }
}

#[tokio::test]
async fn provider_failures_map_status_without_exposing_messages() {
    for (code, expected) in [
        ("unauthenticated", 401),
        ("forbidden", 401),
        ("not_found", 404),
        ("target_conflict", 404),
        ("invalid_arguments", 400),
        ("internal_error", 503),
    ] {
        let mut reply = Reply::default();
        dashboard_image::handle(
            "GET",
            &QUERY.into(),
            None,
            |_, _| std::future::ready(Err(IvyError::new(code, "private provider diagnostic"))),
            &mut reply,
        )
        .await
        .unwrap();
        assert_eq!(reply.status, expected);
        assert_eq!(reply.headers["Cache-Control"], "no-store");
        assert!(!String::from_utf8_lossy(&reply.body).contains("private provider"));
        if expected == 503 {
            assert_eq!(reply.headers["Retry-After"], "5");
            assert!(String::from_utf8_lossy(&reply.body).contains("image_unavailable"));
        }
    }
}

#[tokio::test]
async fn malformed_images_and_metadata_are_unavailable() {
    let cases = [
        ("mimeType", "text/html".into()),
        ("encoding", "binary".into()),
        ("data", "bad base64".into()),
        ("data", STANDARD.encode([0; 33]).into()),
        ("metadata", J::object([("refreshSeconds", J::Number(1.5))])),
        (
            "metadata",
            J::object([
                ("refreshSeconds", J::Number(0.0)),
                ("tooLarge", "x".repeat(8200).into()),
            ]),
        ),
    ];
    for (key, value) in cases {
        let mut value_image = image();
        value_image.insert(key, value);
        let mut reply = Reply::default();
        dashboard_image::handle(
            "GET",
            &QUERY.into(),
            None,
            |_, _| std::future::ready(Ok(value_image)),
            &mut reply,
        )
        .await
        .unwrap();
        assert_eq!(reply.status, 503);
        assert_eq!(reply.headers["Cache-Control"], "no-store");
    }
}
