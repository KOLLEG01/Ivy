use super::*;
use ivy_contracts::{
    IvyJson as J,
    json::{parse, stringify},
};
use std::{
    collections::VecDeque,
    net::{Ipv4Addr, TcpListener},
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};
use tokio::{net::TcpStream, sync::oneshot};

fn obj<const N: usize>(pairs: [(&str, J); N]) -> J {
    J::object(pairs)
}
fn num(n: impl Into<f64>) -> J {
    J::Number(n.into())
}
fn string(v: &J, key: &str) -> String {
    v.get(key).unwrap().as_str().unwrap()
}
fn integer(v: &J, key: &str) -> i64 {
    v.get(key).unwrap().as_f64().unwrap() as i64
}
fn optional(v: &J, key: &str) -> Option<String> {
    v.get(key)
        .filter(|v| !v.is_null())
        .map(|v| v.as_str().unwrap())
}
fn port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn settings(port: u16) -> NativeMicroSettings {
    let raw = format!(
        r#"{{"usbipExecutable":"C:\\owned\\usbip.exe","executableHash":"sha256:{}","port":{port}}}"#,
        "0".repeat(64)
    );
    crate::native_config::NativeMicroValue::read(&parse(&raw, 65536).unwrap(), &raw)
        .unwrap()
        .validate()
        .unwrap()
}
fn inventory(port: u16) -> String {
    format!(
        "Port 01: device in use\n -> usbip://127.0.0.1:{port}/1-7\n -> remote bus/dev: 001/007\n -> serial: IvyPhoneMicro\n"
    )
}
fn normalized(args: Vec<String>, port: u16) -> J {
    J::Array(
        args.into_iter()
            .map(|v| {
                if v == port.to_string() {
                    "<port>".into()
                } else {
                    v.into()
                }
            })
            .collect(),
    )
}
fn state(a: &MicroAttachment) -> J {
    let s = a.attachment_status();
    obj([
        ("state", s.state.into()),
        (
            "generation",
            s.generation.map_or(J::Null, |g| num(g as f64)),
        ),
        ("errorCode", s.error_code.map_or(J::Null, J::from)),
    ])
}
fn trace(name: &str, value: &J) {
    if let Ok(dir) = std::env::var("IVY_PHONE_MICRO_ATTACHMENT_TRACE") {
        std::fs::write(
            std::path::Path::new(&dir).join(name),
            stringify(value, 4 * 1024 * 1024).unwrap(),
        )
        .unwrap();
    }
}
fn fixture(mode: &str, cases: &[J]) -> J {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/phone-micro-attachment-cases.json");
    let data = parse(&std::fs::read_to_string(path).unwrap(), 4 * 1024 * 1024).unwrap();
    let row = data.get(mode).unwrap();
    assert_eq!(
        row.get("cases").unwrap(),
        &J::Array(cases.to_vec()),
        "{mode} input corpus"
    );
    row.get("expected").unwrap().clone()
}
async fn until(mut done: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(25), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("Owned attachment condition timed out");
}
fn reply(started: bool, completed: bool, exit: Option<i32>, output: &str, code: Option<&str>) -> J {
    obj([
        ("started", started.into()),
        ("completed", completed.into()),
        ("exitCode", exit.map_or(J::Null, |n| num(n as f64))),
        ("output", output.into()),
        ("errorCode", code.map_or(J::Null, J::from)),
    ])
}
fn fault(kind: &str) -> J {
    obj([("fault", kind.into())])
}
fn client_reply(reply: &J, port: u16) -> MicroClientResult {
    MicroClientResult {
        started: reply.get("started").unwrap().as_bool().unwrap(),
        completed: reply.get("completed").unwrap().as_bool().unwrap(),
        exit_code: reply
            .get("exitCode")
            .filter(|v| !v.is_null())
            .map(|v| v.as_f64().unwrap() as i32),
        output: match optional(reply, "output").as_deref() {
            Some("own") => inventory(port),
            Some("garbage") => "unrecognized".into(),
            _ => String::new(),
        },
        error_code: optional(reply, "errorCode"),
    }
}
fn row(name: &str, replies: Vec<J>, state: &str, code: Option<&str>, delays: &[u32]) -> J {
    obj([
        ("name", name.into()),
        ("replies", J::Array(replies)),
        ("state", state.into()),
        ("code", code.map_or(J::Null, J::from)),
        ("commands", num(delays.len() as u32)),
        (
            "minimumDelays",
            J::Array(delays.iter().map(|&v| num(v)).collect()),
        ),
    ])
}
fn cases() -> Vec<J> {
    let empty = || reply(true, true, Some(0), "", None);
    let own = || reply(true, true, Some(0), "own", None);
    let unknown = || reply(true, false, None, "", Some("ignored"));
    vec![
        row("already-attached", vec![own()], "attached", None, &[0]),
        row(
            "confirm-success",
            vec![empty(), empty(), own()],
            "attached",
            None,
            &[0, 0, 1000],
        ),
        row(
            "query-unstarted",
            vec![reply(false, false, None, "", Some("owned_client_failure"))],
            "unavailable",
            Some("owned_client_failure"),
            &[0],
        ),
        row(
            "query-no-code",
            vec![reply(false, false, None, "", None)],
            "unavailable",
            Some("micro_query_failed"),
            &[0],
        ),
        row(
            "query-nonzero",
            vec![reply(true, true, Some(2), "", Some("owned_query_failure"))],
            "unavailable",
            Some("owned_query_failure"),
            &[0],
        ),
        row(
            "query-sync-error",
            vec![fault("sync")],
            "unavailable",
            Some("micro_query_failed"),
            &[0],
        ),
        row(
            "query-async-error",
            vec![fault("async")],
            "unavailable",
            Some("micro_query_failed"),
            &[0],
        ),
        row(
            "query-cancel-without-stop",
            vec![fault("cancel")],
            "unavailable",
            Some("micro_query_failed"),
            &[0],
        ),
        row(
            "inventory-conflict",
            vec![reply(true, true, Some(0), "garbage", None)],
            "unavailable",
            Some("micro_attachment_conflict"),
            &[0],
        ),
        row(
            "attach-unstarted",
            vec![
                empty(),
                reply(false, false, None, "", Some("owned_attach_failure")),
            ],
            "starting",
            Some("owned_attach_failure"),
            &[0, 0],
        ),
        row(
            "attach-nonzero",
            vec![empty(), reply(true, true, Some(2), "", None)],
            "starting",
            Some("micro_attach_failed"),
            &[0, 0],
        ),
        row(
            "attach-no-code",
            vec![empty(), reply(false, false, None, "", None)],
            "starting",
            Some("micro_attach_failed"),
            &[0, 0],
        ),
        row(
            "attach-sync-error",
            vec![empty(), fault("sync")],
            "outcome_unknown",
            Some("micro_attach_unknown"),
            &[0, 0],
        ),
        row(
            "attach-async-error",
            vec![empty(), fault("async")],
            "outcome_unknown",
            Some("micro_attach_unknown"),
            &[0, 0],
        ),
        row(
            "unknown-waits-for-inventory",
            vec![empty(), unknown(), empty(), own()],
            "attached",
            None,
            &[0, 0, 1000, 2000],
        ),
        row(
            "unknown-query-failure",
            vec![
                empty(),
                unknown(),
                reply(false, false, None, "", Some("ignored")),
            ],
            "outcome_unknown",
            Some("micro_attach_unknown"),
            &[0, 0, 1000],
        ),
        row(
            "unknown-survives-conflict",
            vec![
                empty(),
                unknown(),
                reply(true, true, Some(0), "garbage", None),
                empty(),
                own(),
            ],
            "attached",
            None,
            &[0, 0, 1000, 2000, 4000],
        ),
        row(
            "retry-known-failure",
            vec![
                empty(),
                reply(false, false, None, "", None),
                empty(),
                empty(),
                own(),
            ],
            "attached",
            None,
            &[0, 0, 1000, 0, 2000],
        ),
        row(
            "attached-poll-delay",
            vec![own(), own()],
            "attached",
            None,
            &[0, 15000],
        ),
    ]
}
async fn matrix_row(row: J) -> J {
    let port = port();
    let replies = Arc::new(Mutex::new(VecDeque::from(
        row.get("replies").unwrap().as_array().unwrap().to_vec(),
    )));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let times = Arc::new(Mutex::new(Vec::new()));
    let watch = Instant::now();
    let (c, t, q) = (calls.clone(), times.clone(), replies.clone());
    let attachment = MicroAttachment::with_executor(
        &settings(port),
        move |args, stop| {
            c.lock().unwrap().push(normalized(args, port));
            t.lock().unwrap().push(watch.elapsed());
            let Some(reply) = q.lock().unwrap().pop_front() else {
                return Box::pin(async move {
                    stop.cancelled().await;
                    Err(MicroCommandError::Cancelled)
                });
            };
            match optional(&reply, "fault").as_deref() {
                Some("sync") => panic!("Owned command fault"),
                Some("async") => Box::pin(async { Err(failed()) }),
                Some("cancel") => Box::pin(async { Err(MicroCommandError::Cancelled) }),
                _ => Box::pin(std::future::ready(Ok(client_reply(&reply, port)))),
            }
        },
        None,
    )
    .unwrap();
    let prefix = calls.lock().unwrap().len();
    let initial = state(&attachment);
    until(|| {
        calls.lock().unwrap().len() >= integer(&row, "commands") as usize
            && attachment.attachment_status().state == string(&row, "state")
            && attachment.attachment_status().error_code == optional(&row, "code")
    })
    .await;
    let observed = state(&attachment);
    let predicates = AtomicUsize::new(0);
    let input = attachment
        .send_microphone(true, 19, &|| {
            predicates.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        })
        .unwrap();
    let voice = attachment.observe_voice(19).unwrap();
    let close = attachment.disposal().unwrap();
    let same = close.same_work(&attachment.disposal().unwrap());
    tokio::time::timeout(Duration::from_secs(3), close.wait())
        .await
        .unwrap()
        .unwrap();
    let commands = calls.lock().unwrap().clone();
    let times = times.lock().unwrap();
    let delays = row.get("minimumDelays").unwrap().as_array().unwrap();
    assert_eq!(times.len(), delays.len());
    for i in 1..times.len() {
        assert!(
            times[i] - times[i - 1] + Duration::from_millis(100)
                >= Duration::from_millis(delays[i].as_f64().unwrap() as u64)
        );
    }
    obj([
        ("name", row.get("name").unwrap().clone()),
        ("prefix", num(prefix as u32)),
        ("initial", initial),
        ("observed", observed),
        ("commands", J::Array(commands)),
        ("predicates", num(predicates.load(Ordering::SeqCst) as u32)),
        ("input", input.to_json()),
        (
            "voice",
            obj([
                ("state", voice.state.into()),
                ("generation", num(voice.generation as f64)),
                ("dictationSequence", num(voice.dictation_sequence as f64)),
            ]),
        ),
        ("same", same.into()),
        ("closed", state(&attachment)),
    ])
}
#[test]
fn inventory_unknown_backoff_and_constructor_prefix_follow_public_cases() {
    let rows = cases();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let actual = runtime.block_on(async {
        let jobs = rows
            .iter()
            .cloned()
            .map(|row| tokio::spawn(matrix_row(row)))
            .collect::<Vec<_>>();
        let mut results = Vec::new();
        for job in jobs {
            results.push(job.await.unwrap());
        }
        J::Array(results)
    });
    trace("matrix-rust.json", &actual);
    let expected = fixture("matrix", &rows);
    assert_eq!(actual, expected);
    trace(
        "matrix-comparisons.json",
        &obj([
            ("cases", num(rows.len() as u32)),
            ("rust", actual),
            ("expected", expected),
        ]),
    );
}
async fn lifetime() -> J {
    let mut results = Vec::new();
    for missing in [false, true] {
        let port = port();
        let (tx, rx) = oneshot::channel();
        let held = Arc::new(Mutex::new(Some(rx)));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let cancelled = Arc::new(Mutex::new(Vec::new()));
        let (h, c, t) = (held.clone(), calls.clone(), cancelled.clone());
        let a = MicroAttachment::with_executor(
            &settings(port),
            move |args, token| {
                c.lock().unwrap().push(normalized(args, port));
                t.lock().unwrap().push(J::Bool(token.is_cancelled()));
                let rx = h.lock().unwrap().take();
                Box::pin(async move {
                    match rx {
                        Some(rx) => Ok(rx.await.unwrap()),
                        None => Err(MicroCommandError::Cancelled),
                    }
                })
            },
            None,
        )
        .unwrap();
        let first = a.disposal().unwrap();
        let same = first.same_work(&a.disposal().unwrap());
        let pending = first.result().is_none();
        let before = state(&a);
        let predicates = AtomicUsize::new(0);
        let input = a
            .send_microphone(true, 19, &|| {
                predicates.fetch_add(1, Ordering::SeqCst);
                Ok(true)
            })
            .unwrap();
        tx.send(MicroClientResult {
            started: true,
            completed: true,
            exit_code: Some(0),
            output: if missing {
                String::new()
            } else {
                inventory(port)
            },
            error_code: None,
        })
        .unwrap();
        tokio::time::timeout(Duration::from_secs(3), first.wait())
            .await
            .unwrap()
            .unwrap();
        results.push(obj([
            (
                "name",
                if missing {
                    "close-query-empty"
                } else {
                    "close-query-owned"
                }
                .into(),
            ),
            ("same", same.into()),
            ("pending", pending.into()),
            ("before", before),
            ("predicates", num(predicates.load(Ordering::SeqCst) as u32)),
            ("phase", input.phase.into()),
            ("commands", J::Array(calls.lock().unwrap().clone())),
            ("cancelled", J::Array(cancelled.lock().unwrap().clone())),
            ("closed", state(&a)),
        ]));
    }
    {
        let port = port();
        let (tx1, rx1) = oneshot::channel();
        let (tx2, rx2) = oneshot::channel();
        let held = Arc::new(Mutex::new(VecDeque::from([rx1, rx2])));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (h, c) = (held.clone(), calls.clone());
        let a = MicroAttachment::with_executor(
            &settings(port),
            move |args, _| {
                c.lock().unwrap().push(normalized(args, port));
                let rx = h.lock().unwrap().pop_front().unwrap();
                Box::pin(async move { Ok(rx.await.unwrap()) })
            },
            None,
        )
        .unwrap();
        let old = a.device();
        let connection = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        until(|| old.last_generation() == 1).await;
        old.disposal().unwrap().wait().await.unwrap();
        tx1.send(MicroClientResult {
            started: false,
            completed: false,
            exit_code: None,
            output: String::new(),
            error_code: Some("owned_query_failure".into()),
        })
        .unwrap();
        until(|| calls.lock().unwrap().len() == 2).await;
        let current = a.device();
        let carried = current.last_generation();
        let before = state(&a);
        let replacement = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        until(|| current.last_generation() == 2).await;
        tx2.send(MicroClientResult {
            started: true,
            completed: true,
            exit_code: Some(0),
            output: inventory(port),
            error_code: None,
        })
        .unwrap();
        until(|| a.attachment_status().state == "attached").await;
        let observed = state(&a);
        let close = a.disposal().unwrap();
        let same = close.same_work(&a.disposal().unwrap());
        tokio::time::timeout(Duration::from_secs(3), close.wait())
            .await
            .unwrap()
            .unwrap();
        results.push(obj([
            ("name", "recover-owned-listener".into()),
            (
                "replaced",
                (!old.healthy() && current.last_generation() == 2).into(),
            ),
            ("carried", num(carried as f64)),
            ("generation", num(current.last_generation() as f64)),
            ("before", before),
            ("observed", observed),
            ("same", same.into()),
            ("commands", J::Array(calls.lock().unwrap().clone())),
            ("closed", state(&a)),
        ]));
        drop((connection, replacement));
    }
    J::Array(results)
}
#[test]
fn recovery_and_cached_disposal_join_the_monitor() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let actual = runtime.block_on(lifetime());
    trace("lifetime-rust.json", &actual);
    let expected = fixture("lifetime", &[]);
    assert_eq!(actual, expected);
    trace(
        "lifetime-comparisons.json",
        &obj([
            ("cases", num(3u32)),
            ("rust", actual),
            ("expected", expected),
        ]),
    );
}
