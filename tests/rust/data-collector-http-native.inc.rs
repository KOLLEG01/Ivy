use super::*;

fn case(kind: &str, changes: &str) -> IvyJson {
    let mut input = if kind == "ingress" {
        json(r#"{"kind":"ingress","method":"POST","path":"/tasks/task/run","headers":["Authorization","Bearer synthetic","Idempotency-Key","run"],"body":"{\"ok\":true}"}"#)
    } else {
        json(r#"{"kind":"mcp","method":"POST","path":"/call","headers":["Authorization","Bearer TOKEN"],"body":"{\"tool\":\"echo\",\"arguments\":{\"ok\":true}}"}"#)
    };
    for (key, value) in json(changes).as_object().unwrap() {
        input.insert(key.clone(), value.clone());
    }
    input
}

#[tokio::test]
async fn ingress_preserves_routes_authentication_idempotency_and_body_limits() {
    for (name, changes, status, count) in [
        ("normal", "{}", 202, 1),
        ("alias", r#"{"path":"/webhook"}"#, 202, 1),
        ("chunked", r#"{"chunked":true}"#, 202, 1),
        ("repeat", r#"{"repeat":true}"#, 202, 1),
        ("missing-auth", r#"{"headers":[]}"#, 401, 0),
        ("wrong-auth", r#"{"headers":["Authorization","Bearer wrong"]}"#, 401, 0),
        ("disabled", r#"{"disabled":true}"#, 401, 0),
        ("public", r#"{"public":true,"headers":["Idempotency-Key","run"]}"#, 202, 1),
        ("method", r#"{"method":"GET"}"#, 401, 0),
        ("query", r#"{"path":"/tasks/task/run?x=1"}"#, 401, 0),
        ("malformed-json", r#"{"body":"{"}"#, 400, 0),
        ("invalid-utf8", r#"{"bodyKind":"invalid"}"#, 400, 0),
        ("exact-limit", r#"{"bodyKind":"large","size":262144}"#, 202, 1),
        ("over-limit", r#"{"bodyKind":"large","size":262145}"#, 413, 0),
        ("busy", r#"{"busy":true}"#, 409, 1),
    ] {
        let (reply, queued, _, _) = observe_http(&case("ingress", changes)).await;
        assert_eq!(model::number(&reply, "status").unwrap(), f64::from(status), "{name}: {reply:?}");
        assert_eq!(queued.as_array().unwrap().len(), count, "{name}");
    }
}

#[tokio::test]
async fn mcp_bridge_preserves_authentication_dispatch_errors_and_budgets() {
    for (name, changes, status, calls) in [
        ("normal", "{}", 200, 1),
        ("chunked", r#"{"chunked":true}"#, 200, 1),
        ("wrong-path", r#"{"path":"/call?x=1"}"#, 401, 0),
        ("wrong-auth", r#"{"headers":["Authorization","Bearer wrong"]}"#, 401, 0),
        ("method", r#"{"method":"GET"}"#, 401, 0),
        ("missing-tool", r#"{"body":"{}"}"#, 400, 0),
        ("malformed-json", r#"{"body":"{"}"#, 400, 0),
        ("invalid-utf8", r#"{"bodyKind":"invalid-mcp"}"#, 400, 0),
        ("provider-error", r#"{"body":"{\"tool\":\"fail\"}"}"#, 400, 1),
        ("large-result", r#"{"body":"{\"tool\":\"large\"}"}"#, 400, 1),
        // The input is admitted; the larger response envelope exceeds its output budget.
        ("exact-limit", r#"{"bodyKind":"large-mcp","size":1048576}"#, 400, 1),
        ("over-limit", r#"{"bodyKind":"large-mcp","size":1048577}"#, 400, 0),
    ] {
        let (reply, _, observed_calls, _) = observe_http(&case("mcp", changes)).await;
        assert_eq!(model::number(&reply, "status").unwrap(), f64::from(status), "{name}: {reply:?}");
        assert_eq!(observed_calls, calls, "{name}");
        if name == "normal" || name == "chunked" {
            let result = reply.required("body").unwrap().required("result").unwrap();
            assert_eq!(result.get("tool"), Some(&"echo".into()));
            assert_eq!(result.get("args"), Some(&json(r#"{"ok":true}"#)));
        }
    }
}

#[tokio::test]
async fn native_http_rejects_oversized_headers_and_ambiguous_framing() {
    let huge = format!("POST /tasks/task/run HTTP/1.1\r\nHost: localhost\r\nX-Large: {}\r\n\r\n", "x".repeat(17_000));
    for (request, status) in [
        (huge.as_str(), 431),
        ("POST /tasks/task/run HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\nContent-Length: 3\r\nConnection: close\r\n\r\n{}", 400),
        ("POST /tasks/task/run HTTP/1.1\r\nAuthorization: Bearer synthetic\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}", 400),
    ] {
        let (_directory, engine, ingress) = fixture(&json("{}")).await;
        let mut stream = TcpStream::connect(ingress.address()).await.unwrap();
        let _ = stream.write_all(request.as_bytes()).await;
        let reply = read_reply(stream).await;
        assert_eq!(model::number(&reply, "status").unwrap(), f64::from(status));
        assert_eq!(engine.store().call(|store| Ok(store.runs("queued")?.len())).await.unwrap(), 0);
        ingress.close().await;
        engine.close().await;
    }
}

#[tokio::test]
async fn native_http_bounds_incomplete_headers_and_bodies() {
    let header = async {
        let (_directory, engine, ingress) = fixture(&json("{}")).await;
        let mut stream = TcpStream::connect(ingress.address()).await.unwrap();
        stream.write_all(b"POST /tasks/task/run HTTP/1.1\r\nHost:").await.unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(13), read_reply(stream)).await.unwrap();
        // Hyper closes an incomplete request head without synthesizing a reply.
        assert_eq!(reply.get("status"), Some(&IvyJson::Null));
        assert_eq!(engine.store().call(|store| Ok(store.runs("queued")?.len())).await.unwrap(), 0);
        ingress.close().await;
        engine.close().await;
    };
    let body = async {
        let (_directory, engine, ingress) = fixture(&json("{}")).await;
        let mut stream = TcpStream::connect(ingress.address()).await.unwrap();
        stream.write_all(b"POST /tasks/task/run HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAuthorization: Bearer synthetic\r\nContent-Length: 2\r\n\r\n{").await.unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(18), read_reply(stream)).await.unwrap();
        assert_eq!(model::number(&reply, "status").unwrap(), 408.0);
        assert_eq!(engine.store().call(|store| Ok(store.runs("queued")?.len())).await.unwrap(), 0);
        ingress.close().await;
        engine.close().await;
    };
    tokio::join!(header, body);
}
