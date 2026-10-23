use super::*;
use futures_util::poll;
use ivy_sdk::rpc::OptionalRpcClient;
use ivy_task_board::journal_client::JournalClient;
use std::{task::Poll, time::Duration};
use tokio::sync::oneshot;

struct Sent {
    method: String,
    params: Option<Json>,
    options: RequestOptions,
    reply: Option<oneshot::Sender<IvyResult<Json>>>,
}
#[derive(Clone, Default)]
struct HeldClient(Arc<Mutex<Vec<Sent>>>);
impl HeldClient {
    fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }
    fn complete(&self, index: usize, result: IvyResult<Json>) {
        self.0.lock().unwrap()[index]
            .reply
            .take()
            .unwrap()
            .send(result)
            .unwrap();
    }
}
impl RpcClient for HeldClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        self.request_optional(method, Some(params), options)
    }
}
impl OptionalRpcClient for HeldClient {
    fn request_optional<'a>(
        &'a self,
        method: &'a str,
        params: Option<Json>,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let (reply, result) = oneshot::channel();
            self.0.lock().unwrap().push(Sent {
                method: method.into(),
                params,
                options: options.clone(),
                reply: Some(reply),
            });
            tokio::select! {
                biased;
                _ = options.cancellation.cancelled() => Err(options.cancel_error),
                result = result => result.unwrap(),
            }
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn journal_reads_writes_and_providers_share_admission_without_read_starvation() {
    let held = HeldClient::default();
    let client = Arc::new(JournalClient::new(held.clone()).unwrap());
    let db = Connection::open_in_memory().unwrap();
    local_journal::initialize(&db).unwrap();
    let store = JournalStore {
        journal: LocalJournal {
            connection: &db,
            root_object_id: Some("root".into()),
        },
        client: client.clone(),
        contracts: FixtureContracts {
            local: HashSet::new(),
            allowed: HashSet::new(),
        },
        limits: HistoryLimits::default(),
        now: || -> String { panic!("A remote page must not read the clock") },
        new_id: || -> JsString { panic!("A remote page must not allocate an identity") },
        cache: None,
    };
    let mut providers = Vec::new();
    for index in 0..16 {
        let mut request = if index % 2 == 0 {
            client.request(
                "tools.call",
                Json::Number(index as f64),
                RequestOptions::default(),
            )
        } else {
            client.request_optional("discovery.call", None, RequestOptions::default())
        };
        assert!(poll!(request.as_mut()).is_pending());
        providers.push(request);
    }
    let options = Json::object([("contractVersions", Json::Null)]);
    let mut reads = Vec::new();
    for _ in 0..31 {
        let mut request = Box::pin(store.remote_page("task-board/task", &options, &[]));
        assert!(poll!(request.as_mut()).is_pending());
        reads.push(request);
    }
    let write_params = Json::object([("mutationId", "unchanged-write".into())]);
    let mut write = client.request(
        "objects.write",
        write_params.clone(),
        RequestOptions::default(),
    );
    assert!(poll!(write.as_mut()).is_pending());
    assert_eq!(held.count(), 48);

    let provider_params = Json::object([("operationId", "queued-provider".into())]);
    let mut provider = client.request(
        "discovery.call",
        provider_params.clone(),
        RequestOptions::default(),
    );
    assert!(poll!(provider.as_mut()).is_pending());
    let mut read = Box::pin(store.remote_page("task-board/task", &options, &[]));
    assert!(poll!(read.as_mut()).is_pending());
    assert_eq!(held.count(), 48);

    let page = Json::object([("items", Json::Array(vec![])), ("nextCursor", Json::Null)]);
    held.complete(16, Ok(page.clone()));
    assert_eq!(reads.remove(0).await.unwrap(), page);
    assert!(poll!(provider.as_mut()).is_pending());
    assert!(poll!(read.as_mut()).is_pending());
    assert_eq!(held.count(), 49);
    {
        let sent = held.0.lock().unwrap();
        assert_eq!(sent[48].method, "objects.query");
        assert_eq!(sent[48].params, sent[16].params);
        assert_eq!(sent[47].method, "objects.write");
        assert_eq!(sent[47].params, Some(write_params));
        assert_eq!(sent[1].params, None);
    }

    let failure = IvyError::new("outcome_unknown", "Original provider failure")
        .with_outcome(Outcome::Unknown);
    held.complete(0, Err(failure.clone()));
    assert_eq!(
        providers.remove(0).await.unwrap_err().to_wire(),
        failure.to_wire()
    );
    assert!(poll!(provider.as_mut()).is_pending());
    assert_eq!(held.count(), 50);
    {
        let sent = held.0.lock().unwrap();
        assert_eq!(sent[49].method, "discovery.call");
        assert_eq!(sent[49].params, Some(provider_params));
    }
    held.complete(48, Ok(page.clone()));
    assert_eq!(read.await.unwrap(), page);
    held.complete(49, Ok(Json::Bool(true)));
    assert_eq!(provider.await.unwrap(), Json::Bool(true));
    assert_eq!(
        held.count(),
        50,
        "Admission must never retry a sent request"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn journal_queue_cancellation_overflow_and_dropped_requests_preserve_capacity() {
    let held = HeldClient::default();
    let client = Arc::new(JournalClient::new(held.clone()).unwrap());
    assert!(client.service_connection().is_none());
    let mut active = Vec::new();
    for _ in 0..48 {
        let mut request = client.request("objects.read", Json::Null, RequestOptions::default());
        assert!(poll!(request.as_mut()).is_pending());
        active.push(request);
    }
    let reason = IvyError::new_js(JsString(vec![0xd800]), JsString(vec![0xdfff]))
        .with_details(Json::object([("original", true.into())]));
    let cancelled = RequestOptions {
        cancel_error: reason.clone(),
        ..RequestOptions::default()
    };
    let mut queued = Vec::new();
    for index in 0..512 {
        let mut request = client.request(
            "objects.read",
            Json::Number(index as f64),
            cancelled.clone(),
        );
        assert!(poll!(request.as_mut()).is_pending());
        queued.push(request);
    }
    let overflow = client
        .request("objects.write", Json::Null, RequestOptions::default())
        .await
        .unwrap_err();
    assert_eq!(overflow.code, "limit_exceeded");
    assert_eq!(overflow.message, "Hive client request queue is full.");
    assert_eq!(overflow.outcome, Outcome::NotExecuted);
    assert_eq!(
        overflow.details,
        Some(Json::object([
            ("budget", "client_queue".into()),
            ("active", Json::Number(512.0)),
            ("limit", Json::Number(512.0)),
        ]))
    );
    assert_eq!(held.count(), 48);
    assert_eq!(
        client
            .request_optional("system.status", None, RequestOptions::default())
            .await
            .unwrap_err()
            .to_wire(),
        overflow.to_wire()
    );

    cancelled.cancellation.cancel();
    for request in queued.drain(..) {
        assert_eq!(request.await.unwrap_err().to_wire(), reason.to_wire());
    }
    assert_eq!(
        client
            .request("objects.read", Json::Null, cancelled.clone())
            .await
            .unwrap_err()
            .to_wire(),
        reason.to_wire()
    );
    for _ in 0..512 {
        let mut request = client.request("objects.read", Json::Null, RequestOptions::default());
        assert!(poll!(request.as_mut()).is_pending());
        queued.push(request);
    }
    assert_eq!(
        held.count(),
        48,
        "Cancelled waiters must never reach the transport"
    );
    drop(queued);
    drop(active);

    let options = RequestOptions {
        timeout: Duration::from_millis(7123),
        cancel_error: reason.clone(),
        ..RequestOptions::default()
    };
    let cancellation = options.cancellation.clone();
    let params = Json::String(JsString(vec![0, 0xd800, 0xdfff]));
    let mut recovered = client.request("tools.call", params.clone(), options);
    assert!(poll!(recovered.as_mut()).is_pending());
    assert_eq!(held.count(), 49);
    {
        let sent = held.0.lock().unwrap();
        assert_eq!(sent[48].method, "tools.call");
        assert_eq!(sent[48].params, Some(params));
        assert_eq!(sent[48].options.timeout, Duration::from_millis(7123));
        assert_eq!(sent[48].options.cancel_error.to_wire(), reason.to_wire());
    }
    cancellation.cancel();
    assert_eq!(recovered.await.unwrap_err().to_wire(), reason.to_wire());
    let mut successor = client.request("tools.call", Json::Null, RequestOptions::default());
    assert!(matches!(poll!(successor.as_mut()), Poll::Pending));
    held.complete(49, Ok(Json::Bool(true)));
    assert_eq!(successor.await.unwrap(), Json::Bool(true));
    assert_eq!(held.count(), 50);
    let mut omitted = client.request_optional("system.status", None, RequestOptions::default());
    let mut null =
        client.request_optional("system.status", Some(Json::Null), RequestOptions::default());
    assert!(poll!(omitted.as_mut()).is_pending());
    assert!(poll!(null.as_mut()).is_pending());
    {
        let sent = held.0.lock().unwrap();
        assert_eq!(sent[50].params, None);
        assert_eq!(sent[51].params, Some(Json::Null));
    }
    held.complete(50, Ok(Json::Bool(true)));
    held.complete(51, Ok(Json::Bool(false)));
    assert_eq!(omitted.await.unwrap(), Json::Bool(true));
    assert_eq!(null.await.unwrap(), Json::Bool(false));
}
