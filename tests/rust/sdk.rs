use ivy_sdk::{IvyJson, Outcome, admission::RequestQueue, rpc::*};
use tokio_util::sync::CancellationToken;
#[tokio::test]
async fn tool_bindings_refresh_only_definitively_rejected_reads() {
    use ivy_sdk::{IvyError, tools::*};
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };
    struct Scripted {
        steps: Mutex<VecDeque<(&'static str, Result<IvyJson, IvyError>)>>,
        calls: Mutex<Vec<IvyJson>>,
    }
    impl RpcClient for Scripted {
        fn request<'a>(
            &'a self,
            method: &'a str,
            params: IvyJson,
            _options: RequestOptions,
        ) -> RequestFuture<'a> {
            Box::pin(async move {
                let (expected, result) = self
                    .steps
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("unexpected replay");
                assert_eq!(method, expected);
                if method == "tools.call" {
                    self.calls.lock().unwrap().push(params);
                }
                result
            })
        }
    }
    fn page(hash: &str, read: bool) -> IvyJson {
        IvyJson::object([
            (
                "items",
                IvyJson::Array(vec![IvyJson::object([
                    ("qualifiedName", "example.observe".into()),
                    ("definitionHash", hash.into()),
                    (
                        "definition",
                        IvyJson::object([
                            ("interfaceVersion", "1.0.0".into()),
                            (
                                "annotations",
                                IvyJson::object([("readOnlyHint", read.into())]),
                            ),
                        ]),
                    ),
                ])]),
            ),
            (
                "provider",
                IvyJson::object([(
                    "node",
                    IvyJson::object([("serviceNodeId", "selected".into())]),
                )]),
            ),
            ("nextCursor", IvyJson::Null),
        ])
    }
    for (read, outcome, replacement_read) in [
        (true, Outcome::NotExecuted, true),
        (false, Outcome::NotExecuted, true),
        (true, Outcome::Unknown, true),
        (true, Outcome::NotExecuted, false),
    ] {
        let refresh = read && outcome == Outcome::NotExecuted;
        let mut steps = VecDeque::from([
            ("tools.list", Ok(page("old", read))),
            (
                "tools.call",
                Err(IvyError::new("tool_definition_changed", "changed").with_outcome(outcome)),
            ),
        ]);
        if refresh {
            steps.push_back(("tools.list", Ok(page("new", replacement_read))));
            if replacement_read {
                steps.push_back(("tools.call", Ok("result".into())));
            }
        }
        let client = Arc::new(Scripted {
            steps: Mutex::new(steps),
            calls: Mutex::new(Vec::new()),
        });
        let tools = ToolClients::new(client.clone());
        let selected = tools
            .service_tools(
                "selected".into(),
                &[InterfaceRequirement {
                    namespace: "example".into(),
                    interface_version: "1.0.0".into(),
                }],
            )
            .unwrap();
        let name = "example.observe".into();
        let original = selected.binding(&name, None).await.unwrap();
        let args = IvyJson::object([(
            "value",
            IvyJson::String(ivy_contracts::JsString(vec![0xd800])),
        )]);
        let result = selected
            .call(
                &name,
                args.clone(),
                Some("epoch:1:nonce".into()),
                ToolCallOptions::default(),
            )
            .await;
        if refresh && replacement_read {
            assert_eq!(result.unwrap(), IvyJson::from("result"));
        } else {
            assert_eq!(result.unwrap_err().code, "tool_definition_changed");
            assert!(Arc::ptr_eq(
                &original,
                &selected.binding(&name, None).await.unwrap()
            ));
        }
        assert!(client.steps.lock().unwrap().is_empty());
        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.len(), if refresh && replacement_read { 2 } else { 1 });
        for call in calls.iter() {
            assert_eq!(call.get("serviceNodeId"), Some(&IvyJson::from("selected")));
            assert_eq!(
                call.get("operationId"),
                Some(&IvyJson::from("epoch:1:nonce"))
            );
            assert_eq!(call.get("arguments"), Some(&args));
        }
        assert_eq!(
            calls[0].get("expectedDefinitionHash"),
            Some(&IvyJson::from("old"))
        );
        if calls.len() == 2 {
            assert_eq!(
                calls[1].get("expectedDefinitionHash"),
                Some(&IvyJson::from("new"))
            );
        }
    }
}
async fn read_http(socket: &mut tokio::net::TcpStream) -> IvyJson {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    let mut part = [0; 4096];
    loop {
        let count = socket.read(&mut part).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&part[..count]);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..end]).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(|v| v.parse::<usize>().unwrap())
                })
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return ivy_contracts::json::parse(
                    std::str::from_utf8(&bytes[end + 4..end + 4 + length]).unwrap(),
                    100_000,
                )
                .unwrap();
            }
        }
    }
}
async fn reply_http(socket: &mut tokio::net::TcpStream, value: &IvyJson) {
    use tokio::io::AsyncWriteExt;
    let body = ivy_contracts::json::stringify(value, 100_000).unwrap();
    socket
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
}
fn rpc_result(request: &IvyJson, result: IvyJson) -> IvyJson {
    IvyJson::object([
        ("jsonrpc", "2.0".into()),
        ("id", request.required("id").unwrap().clone()),
        ("result", result),
    ])
}
#[tokio::test]
async fn batched_reads_share_work_correlate_reordered_results_and_cancel_independently() {
    use tokio::{net::TcpListener, sync::oneshot};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (arrived, observe) = oneshot::channel();
    let (resume, proceed) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = read_http(&mut socket).await;
        let frames = request.as_array().expect("distinct reads are batched");
        assert_eq!(frames.len(), 2, "duplicate consumers share one item");
        arrived.send(()).unwrap();
        proceed.await.unwrap();
        let result = IvyJson::Array(
            frames
                .iter()
                .rev()
                .map(|frame| {
                    rpc_result(
                        frame,
                        frame
                            .required("params")
                            .unwrap()
                            .required("key")
                            .unwrap()
                            .clone(),
                    )
                })
                .collect(),
        );
        reply_http(&mut socket, &result).await;
    });
    let client = HiveClient::new(
        &format!("http://{address}"),
        ClientOptions {
            batch_reads: true,
            ..ClientOptions::default()
        },
    )
    .unwrap();
    let cancelled = CancellationToken::new();
    let mut tasks = Vec::new();
    for (index, key) in ["a", "a", "b"].into_iter().enumerate() {
        let client = client.clone();
        let token = if index == 0 {
            cancelled.clone()
        } else {
            CancellationToken::new()
        };
        tasks.push(tokio::spawn(async move {
            client
                .request(
                    "objects.read",
                    IvyJson::object([("key", key.into())]),
                    RequestOptions {
                        cancellation: token,
                        ..RequestOptions::default()
                    },
                )
                .await
        }));
    }
    observe.await.unwrap();
    cancelled.cancel();
    assert_eq!(
        tasks.remove(0).await.unwrap().unwrap_err().outcome,
        Outcome::Unknown
    );
    resume.send(()).unwrap();
    for (task, want) in tasks.into_iter().zip(["a", "b"]) {
        assert_eq!(task.await.unwrap().unwrap(), IvyJson::from(want));
    }
    server.await.unwrap();
}
#[tokio::test]
async fn writes_invalidate_pending_observations_without_replaying_them() {
    use tokio::{net::TcpListener, sync::oneshot};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (arrived, observe) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut old, _) = listener.accept().await.unwrap();
        let old_request = read_http(&mut old).await;
        arrived.send(()).unwrap();
        let (mut write, _) = listener.accept().await.unwrap();
        let write_request = read_http(&mut write).await;
        assert_eq!(
            write_request.required("method").unwrap().as_str().unwrap(),
            "objects.write"
        );
        reply_http(&mut write, &rpc_result(&write_request, IvyJson::Null)).await;
        let (mut fresh, _) = listener.accept().await.unwrap();
        let fresh_request = read_http(&mut fresh).await;
        assert_ne!(old_request.get("id"), fresh_request.get("id"));
        reply_http(&mut fresh, &rpc_result(&fresh_request, "new".into())).await;
        reply_http(&mut old, &rpc_result(&old_request, "old".into())).await;
    });
    let client = HiveClient::new(
        &format!("http://{address}"),
        ClientOptions {
            batch_reads: true,
            ..ClientOptions::default()
        },
    )
    .unwrap();
    let first = client.clone();
    let old = tokio::spawn(async move {
        first
            .request(
                "objects.read",
                IvyJson::Object(Vec::new()),
                RequestOptions::default(),
            )
            .await
    });
    observe.await.unwrap();
    client
        .request(
            "objects.write",
            IvyJson::Object(Vec::new()),
            RequestOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        client
            .request(
                "objects.read",
                IvyJson::Object(Vec::new()),
                RequestOptions::default()
            )
            .await
            .unwrap(),
        IvyJson::from("new")
    );
    assert_eq!(old.await.unwrap().unwrap(), IvyJson::from("old"));
    server.await.unwrap();
}
#[tokio::test]
async fn http_exchange_preserves_surrogates_and_does_not_replay_uncertain_work() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for respond in [true, false] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut part = [0; 4096];
            let request = loop {
                let count = socket.read(&mut part).await.unwrap();
                assert_ne!(count, 0);
                bytes.extend_from_slice(&part[..count]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&bytes[..end]).unwrap();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(|v| v.parse::<usize>().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break ivy_contracts::json::parse(
                            std::str::from_utf8(&bytes[end + 4..end + 4 + length]).unwrap(),
                            10000,
                        )
                        .unwrap();
                    }
                }
            };
            if respond {
                let body = ivy_contracts::json::stringify(
                    &IvyJson::object([
                        ("jsonrpc", "2.0".into()),
                        ("id", request.required("id").unwrap().clone()),
                        (
                            "result",
                            IvyJson::String(ivy_contracts::JsString(vec![0xd800])),
                        ),
                    ]),
                    10000,
                )
                .unwrap();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "uncertain work must not be replayed"
        );
    });
    let client = HiveClient::new(&format!("http://{address}"), ClientOptions::default()).unwrap();
    let result = client
        .request(
            "system.status",
            IvyJson::Object(Vec::new()),
            RequestOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(result.as_js_string().unwrap().0, [0xd800]);
    let error = client
        .request(
            "objects.write",
            IvyJson::Object(Vec::new()),
            RequestOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.outcome, Outcome::Unknown);
    server.await.unwrap();
}
#[test]
fn timezone_contact_windows_and_assessment_policy_match_typescript() {
    use ivy_sdk::{assessment::*, clock::*};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/sdk-policy-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases = ivy_contracts::json::parse(std::str::from_utf8(&output.stdout).unwrap(), 1_000_000)
        .unwrap();
    let windows = cases.required("windows").unwrap().to_serde().unwrap();
    for case in windows.as_array().unwrap() {
        let w = &case["window"];
        let window = ContactWindow {
            time_zone: w["timeZone"].as_str().unwrap().into(),
            start: w["start"].as_str().unwrap().into(),
            end: w["end"].as_str().unwrap().into(),
        };
        assert_eq!(
            contact_window_open(Some(&window), case["now"].as_i64().unwrap()).unwrap(),
            case["open"].as_bool().unwrap(),
            "{case}"
        );
    }
    for case in cases.required("assessments").unwrap().as_array().unwrap() {
        let input = case.required("input").unwrap();
        let result = restrict_assessment_parameters(input);
        if let Some(error) = case.get("error") {
            assert_eq!(result.unwrap_err().code, error.as_str().unwrap());
        } else {
            let want = case.required("output").unwrap();
            assert_eq!(&result.unwrap(), want);
            assert!(assert_assessment_policy(want).is_ok());
            assert!(assert_assessment_policy(input).is_err());
        }
    }
}
#[tokio::test]
async fn ordinary_requests_pass_waiting_providers_and_cancelled_waiters_release() {
    let queue = RequestQueue::new(2, 1, 2).unwrap();
    let token = CancellationToken::new();
    let provider = queue.acquire(true, &token).await.unwrap();
    let q = queue.clone();
    let cancelled = CancellationToken::new();
    let child = cancelled.clone();
    let waiting = tokio::spawn(async move { q.acquire(true, &child).await });
    tokio::task::yield_now().await;
    let ordinary = queue.acquire(false, &token).await.unwrap();
    cancelled.cancel();
    assert!(waiting.await.unwrap().is_err());
    drop(ordinary);
    drop(provider);
    assert!(queue.acquire(true, &token).await.is_ok());
}
#[test]
fn response_identity_and_early_http_refusals() {
    let error = IvyJson::object([
        ("jsonrpc", "2.0".into()),
        ("id", IvyJson::Null),
        (
            "error",
            ivy_sdk::IvyError::new("unauthenticated", "No credential").to_wire(),
        ),
    ]);
    assert_eq!(
        response_value(&error, "r").unwrap_err().outcome,
        Outcome::NotExecuted
    );
    let success = IvyJson::object([
        ("jsonrpc", "2.0".into()),
        ("id", "other".into()),
        ("result", IvyJson::Null),
    ]);
    assert_eq!(
        response_value(&success, "r").unwrap_err().code,
        "invalid_frame"
    );
    assert!(base_url("http://remote.invalid/").is_err());
    assert!(base_url("https://host.invalid/a?b").is_err());
    assert_eq!(
        base_url("http://[::1]/a").unwrap().as_str(),
        "http://[::1]/a/"
    );
}
