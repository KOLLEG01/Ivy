use ivy_contracts::{IvyJson, json::parse};
use ivy_data_collector::provider::{CollectorBackend, service_options};
use ivy_sdk::{
    rpc::{RequestOptions, RpcClient},
    service::{InvocationContext, ServiceClient, ServiceFuture, ServiceState},
};
use std::{
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::Semaphore,
};

#[derive(Clone)]
struct Backend {
    calls: Arc<Mutex<Vec<String>>>,
    gate: Arc<Semaphore>,
    client: Arc<Mutex<Option<Arc<dyn RpcClient>>>>,
}
impl CollectorBackend for Backend {
    fn read(&self, input: IvyJson) -> ServiceFuture<IvyJson> {
        Box::pin(async move { Ok(input) })
    }
    fn update(&self, input: IvyJson, context: InvocationContext) -> ServiceFuture<IvyJson> {
        self.calls.lock().unwrap().push("update".into());
        Box::pin(async move {
            Ok(IvyJson::object([
                ("input", input),
                ("caller", IvyJson::String(context.caller_principal_id)),
                ("generation", IvyJson::Number(context.generation as f64)),
            ]))
        })
    }
    fn reconcile(&self, client: Arc<dyn RpcClient>) -> ServiceFuture<()> {
        let this = self.clone();
        Box::pin(async move {
            this.calls.lock().unwrap().push("reconcile".into());
            *this.client.lock().unwrap() = Some(client.clone());
            this.gate.acquire().await.unwrap().forget();
            client
                .request(
                    "test.reconcile",
                    IvyJson::object(Vec::<(String, IvyJson)>::new()),
                    RequestOptions::default(),
                )
                .await?;
            Ok(())
        })
    }
    fn detach(&self) {
        self.calls.lock().unwrap().push("detach".into());
        *self.client.lock().unwrap() = None;
    }
    fn state(&self, state: ServiceState) {
        self.calls.lock().unwrap().push(state.status.into());
    }
}
fn json(text: &str) -> IvyJson {
    parse(text, usize::MAX).unwrap()
}
#[tokio::test]
async fn collector_provider_registers_original_contracts_and_fences_each_generation() {
    tokio::time::timeout(Duration::from_secs(20),async {
        let root=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let mut peer=Command::new("node").args(["--experimental-transform-types","--disable-warning=ExperimentalWarning","tests/fixtures/data-collector-provider-peer.mjs"])
            .current_dir(root).stdout(Stdio::piped()).stderr(Stdio::inherit()).kill_on_drop(true).spawn().unwrap();
        let first=BufReader::new(peer.stdout.take().unwrap()).lines().next_line().await.unwrap().unwrap();
        let url=json(&first).get("url").unwrap().as_str().unwrap();
        let backend=Arc::new(Backend{calls:Arc::new(Mutex::new(Vec::new())),gate:Arc::new(Semaphore::new(0)),client:Arc::new(Mutex::new(None))});
        let identity=json(r#"{"serviceNodeId":"fixture.collector","serviceName":"data-collector","hostId":"fixture","version":"1.0.0","buildId":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","hiveProtocol":1}"#);
        let mut options=service_options(url,Arc::new(||Box::pin(async{Ok("fixture-token".into())})),identity,backend.clone());
        options.reconnect_min=Duration::from_millis(10);options.reconnect_max=Duration::from_millis(20);
        let service=ServiceClient::new(options).unwrap();service.start();
        while !backend.calls.lock().unwrap().iter().any(|s|s=="reconcile"){tokio::time::sleep(Duration::from_millis(5)).await;}
        assert!(!service.ready());backend.gate.add_permits(1);
        let connection=service.wait_ready(None).await.unwrap();
        let info=connection.request("test.info",json("{}"),RequestOptions::default()).await.unwrap();
        let registry=info.get("registry").unwrap();
        assert_eq!(registry.get("contracts").unwrap().as_array().unwrap().len(),7);
        assert_eq!(registry.get("namespaces").unwrap().as_array().unwrap()[0].get("tools").unwrap().as_array().unwrap().len(),2);
        let trace=info.get("trace").unwrap().as_array().unwrap();
        let position=|name:&str|trace.iter().position(|v|v==&IvyJson::from(name)).unwrap();
        assert!(position("registry.sync")<position("test.reconcile"));assert!(position("test.reconcile")<position("service.heartbeat"));
        let read=connection.request("test.invoke",json(r#"{"tool":"read","input":{"view":"tasks","extra":true}}"#),RequestOptions::default()).await.unwrap();
        assert_eq!(read.get("result"),Some(&json(r#"{"view":"tasks"}"#)));
        let mismatch=connection.request("test.invoke",json(r#"{"tool":"update","input":{"action":"import","operationId":"different"}}"#),RequestOptions::default()).await.unwrap();
        assert_eq!(mismatch.get("error").unwrap().get("data").unwrap().get("code"),Some(&"operation_conflict".into()));
        assert!(!backend.calls.lock().unwrap().iter().any(|s|s=="update"));
        let accepted=connection.request("test.invoke",json(r#"{"tool":"update","input":{"action":"import","operationId":"operation-\udfff"}}"#),RequestOptions::default()).await.unwrap();
        assert_eq!(accepted.get("result").unwrap().get("caller"),json(r#"{"value":"caller-\ud800"}"#).get("value"));
        let old=backend.client.lock().unwrap().clone().unwrap();
        connection.request("test.close",json("{}"),RequestOptions::default()).await.unwrap();
        backend.gate.add_permits(1);
        loop { if let Ok(c)=service.connection(){if c.generation()>connection.generation(){break;}}tokio::time::sleep(Duration::from_millis(5)).await; }
        let new=backend.client.lock().unwrap().clone().unwrap();assert!(!Arc::ptr_eq(&old,&new));
        service.stop().await;assert!(backend.client.lock().unwrap().is_none());
        let calls=backend.calls.lock().unwrap();
        assert_eq!(calls.iter().filter(|s|*s=="reconcile").count(),2);
        assert!(calls.windows(2).any(|pair|pair==["detach","offline"]));
        drop(calls);peer.kill().await.unwrap();peer.wait().await.unwrap();
    }).await.unwrap();
}
