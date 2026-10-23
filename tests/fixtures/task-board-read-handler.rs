use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, Outcome,
    json::{parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestOptions, RpcClient};

use futures_util::FutureExt;
use ivy_task_board::engine_read::read_failure::{ReadCause, ReadFailure};
use std::cell::Cell;
fn f<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn text(value: &Json) -> String {
    value.as_str().unwrap()
}
fn yes(value: &Json, key: &str) -> bool {
    value.get(key).and_then(Json::as_bool) == Some(true)
}
use ivy_sdk::service::{ConnectionOptions, InvocationContext, ServiceConnection};
use ivy_task_board::{
    engine_service::{LocalReadFuture, ReadService},
    service_bridge::{LocalHandler, service_bridge},
};
use std::{future::Future, path::Path, process::Stdio, rc::Rc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::Notify,
};

async fn bounded<T>(work: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(8), work)
        .await
        .expect("Read provider fixture did not settle")
}
struct Peer {
    child: Child,
    url: String,
    registry: Json,
    reader: tokio::task::JoinHandle<()>,
}
impl Peer {
    async fn start() -> Self {
        let mut child = Command::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/task-board-service-peer.mjs",
                "--read-handler",
            ])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let config = parse(
            &bounded(lines.next_line()).await.unwrap().unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap();
        let reader =
            tokio::spawn(async move { while lines.next_line().await.unwrap().is_some() {} });
        Self {
            child,
            url: text(f(&config, "url")),
            registry: f(&config, "registry").clone(),
            reader,
        }
    }
    async fn open(
        &self,
        bridge: &ivy_task_board::service_bridge::ServiceBridge,
    ) -> ServiceConnection {
        let mut options = ConnectionOptions::new("fixture-node".into());
        options
            .handlers
            .insert("task-board.read".into(), bridge.handler("read"));
        let connection = bounded(ServiceConnection::connect(
            &self.url,
            "fixture-token",
            options,
            RequestOptions::default().cancellation,
        ))
        .await
        .unwrap();
        connection.set_generation(7);
        connection.set_registry(&self.registry).unwrap();
        connection.set_ready(true);
        connection
    }
    async fn finish(mut self) {
        self.child.kill().await.unwrap();
        self.child.wait().await.unwrap();
        bounded(self.reader).await.unwrap();
    }
}
async fn call(connection: &ServiceConnection, method: &str, params: Json) -> Json {
    bounded(connection.request(method, params, RequestOptions::default()))
        .await
        .unwrap()
}
struct Other(Cell<usize>);
impl LocalHandler for Other {
    async fn handle(&self, tool: &str, args: Json, context: InvocationContext) -> IvyResult<Json> {
        self.0.set(self.0.get() + 1);
        Ok(Json::object([
            ("tool", tool.into()),
            ("arguments", args),
            ("generation", Json::Number(context.generation as f64)),
            ("caller", Json::String(context.caller_principal_id)),
        ]))
    }
}
fn exact(actual: &Json, expected: &Json) {
    assert_eq!(
        ivy_contracts::hash::hash_json(actual).unwrap(),
        ivy_contracts::hash::hash_json(expected).unwrap(),
        "actual: {actual:?}; expected: {expected:?}"
    );
}

// No Clone/Debug/Display/Error/Send/Sync is supplied for this payload.
struct LocalCause {
    identity: Rc<()>,
    dropped: Rc<Cell<bool>>,
    value: Cell<usize>,
}
impl Drop for LocalCause {
    fn drop(&mut self) {
        self.dropped.set(true);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_read_edge_preserves_local_identity_admission_capacity_and_other_tools() {
    let peer = Peer::start().await;
    let (bridge, owner) = service_bridge(7);
    let client = peer.open(&bridge).await;
    let entered = Cell::new(0);
    let release = Cell::new(false);
    let gate = Notify::new();
    let identity = Rc::new(());
    let dropped = Rc::new(Cell::new(false));
    let read = |args: Json, _: InvocationContext| -> LocalReadFuture<'_, LocalCause> {
        let entered = &entered;
        let release = &release;
        let gate = &gate;
        let identity = &identity;
        let dropped = &dropped;
        async move {
            entered.set(entered.get() + 1);
            if yes(&args, "hold") {
                while !release.get() {
                    gate.notified().await;
                }
            }
            if yes(&args, "raw") {
                let error = ReadFailure::source(LocalCause {
                    identity: identity.clone(),
                    dropped: dropped.clone(),
                    value: Cell::new(41),
                });
                let retained = error.clone();
                tokio::task::yield_now().await;
                assert!(retained.ptr_eq(&error));
                match error.cause() {
                    ReadCause::Source(cause) => {
                        assert!(Rc::ptr_eq(&cause.identity, &identity));
                        assert_eq!(cause.value.get(), 41);
                    }
                    _ => unreachable!(),
                }
                return Err(error);
            }
            Ok(args)
        }
        .boxed_local()
    };
    let other = Other(Cell::new(0));
    let service = ReadService {
        read: &read,
        other: &other,
    };
    let work = async {
        let stale = call(
            &client,
            "test.invokeRead",
            Json::object([("arguments", Json::Null), ("generation", Json::Number(6.0))]),
        )
        .await;
        exact(
            f(&stale, "error"),
            &IvyError::new("stale_generation", "Invocation generation is not ready.").to_wire(),
        );
        let changed = call(
            &client,
            "test.invokeRead",
            Json::object([
                ("arguments", Json::Null),
                (
                    "definitionHash",
                    format!("sha256:{}", "b".repeat(64)).into(),
                ),
            ]),
        )
        .await;
        exact(
            f(&changed, "error"),
            &IvyError::new(
                "tool_definition_changed",
                "Invocation does not match this registered definition.",
            )
            .to_wire(),
        );
        client.set_ready(false);
        let unready = call(
            &client,
            "test.invokeRead",
            Json::object([("arguments", Json::Null)]),
        )
        .await;
        exact(f(&unready, "error"), f(&stale, "error"));
        client.set_ready(true);
        assert_eq!(entered.get(), 0);
        let raw = call(
            &client,
            "test.invokeRead",
            Json::object([("arguments", Json::object([("raw", true.into())]))]),
        )
        .await;
        exact(
            f(&raw, "error"),
            &IvyError::new("internal_error", "Service invocation failed.")
                .with_outcome(Outcome::Unknown)
                .to_wire(),
        );
        assert!(dropped.get());
        assert_eq!(Rc::strong_count(&identity), 1);
        let before = entered.get();
        call(
            &client,
            "test.startReadInvocations",
            Json::object([("count", Json::Number(64.0))]),
        )
        .await;
        bounded(async {
            while entered.get() < before + 64 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        let full = call(
            &client,
            "test.invokeRead",
            Json::object([("arguments", Json::Null)]),
        )
        .await;
        exact(
            f(&full, "error"),
            &IvyError::new("limit_exceeded", "Service handler capacity is full.").to_wire(),
        );
        assert_eq!(entered.get(), before + 64);
        release.set(true);
        gate.notify_waiters();
        let passed = bridge.handler("list")(
            Json::object([("retained", true.into())]),
            InvocationContext::detached(
                "other-caller".into(),
                None,
                7,
                RequestOptions::default().cancellation,
                ivy_sdk::IvyError::new("service_unavailable", "Service connection is stopping.")
                    .with_outcome(ivy_sdk::Outcome::Unknown),
            ),
        )
        .await
        .unwrap();
        exact(
            &passed,
            &Json::object([
                ("tool", "list".into()),
                ("arguments", Json::object([("retained", true.into())])),
                ("generation", Json::Number(7.0)),
                ("caller", "other-caller".into()),
            ]),
        );
        assert_eq!(other.0.get(), 1);
        // Retirement drains all64 accepted calls; dropping observers never
        // reclassifies a pre-start capacity refusal as a started raw failure.
        client.close();
        bridge.retire().await.unwrap();
        Json::object([
            ("stale", stale),
            ("changed", changed),
            ("unready", unready),
            ("raw", raw),
            ("full", full),
            ("acceptedCalls", Json::Number(entered.get() as f64)),
            ("opaqueDroppedBeforeReply", dropped.get().into()),
            ("other", passed),
        ])
    };
    let (closed, result) =
        bounded(async { tokio::join!(owner.run(&service, async { Ok(()) }), work) }).await;
    closed.unwrap();
    peer.finish().await;
    if let Ok(path) = std::env::var("IVY_TASK_BOARD_PROVIDER_CONTROLS") {
        std::fs::write(path, stringify(&result, MANAGEMENT_FRAME_BYTES).unwrap()).unwrap();
    }
}
