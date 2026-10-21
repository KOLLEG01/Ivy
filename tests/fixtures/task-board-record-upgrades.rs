use ivy_contracts::{
    IvyError, IvyJson as Json, Outcome,
    error::JavaScriptExceptionKind,
    json::{parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_task_board::journal_store::persistent_reads::record_upgrades::{
    RecordFailure, upgrade_stored,
};
use std::{
    borrow::Cow,
    collections::VecDeque,
    future::Future,
    pin::pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake},
};

fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    value.get(key).unwrap()
}
fn items(value: &Json) -> &[Json] {
    value.as_array().unwrap()
}
fn fixture(variable: &str) -> Json {
    parse(
        &std::fs::read_to_string(std::env::var(variable).unwrap()).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap()
}
fn same(actual: &Json, expected: &Json, name: &str) {
    assert_eq!(
        stringify(actual, MANAGEMENT_FRAME_BYTES).unwrap(),
        stringify(expected, MANAGEMENT_FRAME_BYTES).unwrap(),
        "{name}"
    );
}
struct NoWake;
impl Wake for NoWake {
    fn wake(self: Arc<Self>) {}
}
fn complete<T>(work: impl Future<Output = T>) -> T {
    let mut work = pin!(work);
    let waker = Arc::new(NoWake).into();
    for _ in 0..128 {
        if let Poll::Ready(value) = work.as_mut().poll(&mut Context::from_waker(&waker)) {
            return value;
        }
    }
    panic!("Finite retained RPC fixture blocked")
}
struct Client {
    pending: Mutex<VecDeque<Json>>,
    trace: Mutex<Vec<Json>>,
    error: Mutex<Option<IvyError>>,
}
impl Client {
    fn new(pending: &[Json]) -> Self {
        Self {
            pending: Mutex::new(pending.iter().cloned().collect()),
            trace: Mutex::default(),
            error: Mutex::default(),
        }
    }
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            assert!(!options.cancellation.is_cancelled());
            self.trace.lock().unwrap().push(Json::object([
                ("method", method.into()),
                ("params", params),
            ]));
            let next = self
                .pending
                .lock()
                .unwrap()
                .pop_front()
                .expect("Unexpected RPC");
            if let Some(error) = next.get("error") {
                let mut error_value = IvyError::new_js(
                    field(error, "code").as_js_string().unwrap().clone(),
                    field(error, "message").as_js_string().unwrap().clone(),
                );
                error_value.outcome = match field(error, "outcome").as_str().unwrap().as_str() {
                    "completed" => Outcome::Completed,
                    "unknown" => Outcome::Unknown,
                    _ => Outcome::NotExecuted,
                };
                error_value.details = error.get("details").cloned();
                *self.error.lock().unwrap() = Some(error_value.clone());
                return Err(error_value);
            }
            Ok(field(&next, "value").clone())
        })
    }
}
fn observed(result: Result<Cow<'_, Json>, RecordFailure>) -> Json {
    match result {
        Ok(value) => Json::object([("value", value.into_owned())]),
        Err(RecordFailure::Ivy(error)) => Json::object([("ivy", error.to_wire())]),
        Err(RecordFailure::JavaScript(error)) => Json::object([(
            "javascript",
            Json::object([
                (
                    "kind",
                    match error.kind {
                        JavaScriptExceptionKind::TypeError => "TypeError",
                        JavaScriptExceptionKind::RangeError => "RangeError",
                    }
                    .into(),
                ),
                ("message", Json::String(error.message)),
            ]),
        )]),
    }
}

#[test]
fn actual_shared_upgrades_follow_owned_backfill_without_normalizing_their_failures() {
    let fixture = fixture("IVY_TASK_BOARD_RECORD_UPGRADES_FIXTURE");
    let mut observations = Vec::new();
    for case in items(field(&fixture, "cases")) {
        let name = field(case, "name").as_str().unwrap();
        let input = field(case, "input");
        let before = input.clone();
        let client = Client::new(items(field(input, "rpc")));
        let key = field(input, "key").as_str().unwrap();
        let result = complete(upgrade_stored(&client, &key, field(input, "value")));
        if let (Some(identity), Ok(value)) = (case.get("sameIdentity"), &result) {
            let borrowed = matches!(value, Cow::Borrowed(original) if std::ptr::eq(*original, field(input, "value")));
            assert_eq!(borrowed, identity.as_bool().unwrap(), "{name}");
        }
        if let Some(source_error) = client.error.lock().unwrap().as_ref() {
            match &result {
                Err(RecordFailure::Ivy(error)) => assert_eq!(error, source_error),
                _ => panic!("Typed source failure was replaced: {name}"),
            }
        }
        let value = observed(result);
        same(&value, field(case, "expected"), &name);
        let trace = Json::Array(client.trace.lock().unwrap().clone());
        same(&trace, field(case, "trace"), &name);
        assert_eq!(
            client.pending.lock().unwrap().len() as f64,
            field(case, "rpcRemaining").as_f64().unwrap(),
            "{name}"
        );
        same(input, &before, "Input remains unmodified");
        observations.push(Json::object([
            ("name", name.into()),
            ("result", value),
            ("trace", trace),
        ]));
    }
    assert_eq!(observations.len(), 21);
    if let Ok(path) = std::env::var("IVY_TASK_BOARD_RECORD_UPGRADES_NATIVE") {
        std::fs::write(
            path,
            stringify(&Json::Array(observations), MANAGEMENT_FRAME_BYTES).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn record_dispatch_reuses_retained_original_shared_observations() {
    let fixture = fixture("IVY_TASK_BOARD_RECORD_UPGRADES_REUSED");
    let mut count = 0;
    for case in items(field(&fixture, "cases")) {
        let key = field(case, "key").as_str().unwrap();
        let input = field(case, "input");
        let client = Client::new(&[]);
        let result = complete(upgrade_stored(&client, &key, input));
        if let (Some(identity), Ok(value)) = (case.get("sameIdentity"), &result) {
            let borrowed =
                matches!(value, Cow::Borrowed(original) if std::ptr::eq(*original, input));
            assert_eq!(borrowed, identity.as_bool().unwrap());
        }
        let result = observed(result);
        same(
            &result,
            field(case, "expected"),
            &field(case, "name").as_str().unwrap(),
        );
        assert!(client.trace.lock().unwrap().is_empty());
        assert!(client.pending.lock().unwrap().is_empty());
        count += 1;
    }
    assert_eq!(count as f64, field(&fixture, "count").as_f64().unwrap());
    assert!(count >= 30);
}
