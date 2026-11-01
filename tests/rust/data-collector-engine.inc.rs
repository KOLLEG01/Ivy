use ivy_contracts::{IvyError, IvyJson, IvyResult, json::parse};
use ivy_data_collector::{
    engine::{Engine, RegisterJob, Runner},
    model,
    store::{CollectorStore, Limits},
    store_owner::StoreOwner,
};
use ivy_sdk::{clock::Clock, service::ServiceFuture};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn utc_millis(&self) -> i64 {
        self.0.load(Ordering::Acquire)
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
#[derive(Clone)]
struct TestRunner {
    calls: Arc<Mutex<Vec<IvyJson>>>,
    finish: Arc<Semaphore>,
    proved_stopped: Arc<AtomicBool>,
    observation_failure: Arc<AtomicBool>,
}
impl Runner for TestRunner {
    fn run(
        &self,
        task: IvyJson,
        state: IvyJson,
        input: IvyJson,
        signal: CancellationToken,
        register: RegisterJob,
    ) -> ServiceFuture<IvyJson> {
        let this = self.clone();
        Box::pin(async move {
            let index = {
                let mut calls = this.calls.lock().unwrap();
                calls.push(IvyJson::object([
                    ("task", task),
                    ("state", state),
                    ("input", input),
                ]));
                calls.len()
            };
            register(format!("fixture-job-{index}")).await?;
            tokio::select! {biased;_=signal.cancelled()=>Err(IvyError::new("outcome_unknown","Tree absence is unproven.")),permit=this.finish.acquire()=>{permit.unwrap().forget();Ok(IvyJson::object([("data",true.into()),("state",IvyJson::Number(index as f64))]))}}
        })
    }
    fn stopped(&self, _: String, _: String) -> ServiceFuture<bool> {
        let proved = self.proved_stopped.load(Ordering::Acquire);
        let failure = self.observation_failure.load(Ordering::Acquire);
        Box::pin(async move {
            if failure {
                Err(IvyError::new(
                    "observation_failed",
                    "Retirement is unproven.",
                ))
            } else {
                Ok(proved)
            }
        })
    }
    fn clear_authentication(&self, _: &str) -> IvyResult<()> {
        Ok(())
    }
}
fn json(s: &str) -> IvyJson {
    parse(s, usize::MAX).unwrap()
}
struct Directory(std::path::PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn fixture() -> (Directory, Engine, TestRunner, Arc<TestClock>) {
    let directory = Directory(
        std::env::temp_dir().join(format!("ivy-collector-engine-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir_all(&directory.0).unwrap();
    let store = StoreOwner::new(
        CollectorStore::open(
            &directory.0.join("store.sqlite"),
            Limits::default(),
            "1970-01-01T00:16:40.000Z",
        )
        .unwrap(),
    );
    let runner = TestRunner {
        calls: Arc::default(),
        finish: Arc::new(Semaphore::new(0)),
        proved_stopped: Arc::new(AtomicBool::new(false)),
        observation_failure: Arc::new(AtomicBool::new(false)),
    };
    let clock = Arc::new(TestClock(AtomicI64::new(1000000)));
    let engine = Engine::new(
        store,
        Arc::new(runner.clone()),
        json(r#"{"concurrency":1}"#),
        clock.clone(),
    )
    .await
    .unwrap();
    (directory, engine, runner, clock)
}
async fn save(engine: &Engine, id: &str, enabled: bool) {
    let mut task = json(
        r#"{"id":"task","name":"Task","enabled":false,"intervalSeconds":1,"timeoutSeconds":5,"memoryMb":128,"script":"export default ()=>({data:null})","dependencies":{},"config":{},"secretNames":[],"retention":{"maximumCount":1000,"maximumAgeDays":7,"maximumBytes":104857600}}"#,
    );
    task.insert("id", id.into());
    task.insert("enabled", enabled.into());
    engine
        .update(
            IvyJson::object([
                ("action", "save".into()),
                ("operationId", format!("save-{id}").into()),
                ("expectedRevision", IvyJson::Number(0.0)),
                ("task", task),
            ]),
            None,
        )
        .await
        .unwrap();
}
async fn wait(condition: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn scheduler_keeps_advancing_offline_cursor_while_publication_is_pending() {
    let (_directory, engine, runner, clock) = fixture().await;
    save(&engine, "task", true).await;
    clock.0.store(1001000, Ordering::Release);
    engine.tick(false).await.unwrap();
    wait(|| runner.calls.lock().unwrap().len() == 1).await;
    runner.finish.add_permits(1);
    wait(|| !engine.active("task")).await;
    assert_eq!(engine.pending_publications().await.unwrap(), 1);
    let row = engine
        .read(json(r#"{"view":"task","id":"task"}"#))
        .await
        .unwrap();
    assert_eq!(model::number(&row, "revision").unwrap(), 1.0);
    engine.tick(true).await.unwrap();
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
    clock.0.store(1002000, Ordering::Release);
    engine.tick(false).await.unwrap();
    wait(|| runner.calls.lock().unwrap().len() == 2).await;
    assert_eq!(
        runner.calls.lock().unwrap()[1].get("state"),
        Some(&IvyJson::Number(1.0))
    );
    runner.finish.add_permits(1);
    wait(|| !engine.active("task")).await;
    assert_eq!(engine.pending_publications().await.unwrap(), 2);
    engine.close().await;
}
#[tokio::test]
async fn concurrency_disable_recovery_and_concurrent_close_preserve_job_fences() {
    let (_directory, engine, runner, clock) = fixture().await;
    for id in ["a", "b"] {
        save(&engine, id, false).await;
        engine
            .update(
                IvyJson::object([
                    ("action", "run".into()),
                    ("operationId", format!("run-{id}").into()),
                    ("id", id.into()),
                ]),
                None,
            )
            .await
            .unwrap();
    }
    engine.tick(false).await.unwrap();
    wait(|| runner.calls.lock().unwrap().len() == 1).await;
    assert!(engine.active("a"));
    assert!(!engine.active("b"));
    // Registration is awaited before the runner blocks on its simulated process.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let run = engine
            .store()
            .call(|s| s.run("run-a"))
            .await
            .unwrap()
            .unwrap();
        if run.get("workerJobName").is_some() {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    engine
        .update(
            json(r#"{"action":"disable","operationId":"disable-a","id":"a","expectedRevision":1}"#),
            None,
        )
        .await
        .unwrap();
    wait(|| !engine.active("a")).await;
    let error = engine
        .update(
            json(r#"{"action":"run","operationId":"retry-a","id":"a"}"#),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "outcome_unknown");
    engine.tick(false).await.unwrap();
    wait(|| engine.active("b")).await;
    runner.finish.add_permits(1);
    wait(|| !engine.active("b")).await;
    // This mock supplies positive retained-owner proof, not namespace absence.
    runner.proved_stopped.store(true, Ordering::Release);
    clock.0.store(1003000, Ordering::Release);
    engine.tick(false).await.unwrap();
    engine
        .update(
            json(r#"{"action":"run","operationId":"new-a","id":"a"}"#),
            None,
        )
        .await
        .unwrap();
    engine.tick(false).await.unwrap();
    wait(|| engine.active("a")).await;
    let first = engine.clone();
    let second = engine.clone();
    tokio::join!(first.close(), second.close());
    assert!(!engine.active("a"));
    let run = engine
        .store()
        .call(|s| s.run("new-a"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(run.get("error"), Some(&"outcome_unknown".into()));
    assert!(run.get("workerJobName").is_some());
    let count = runner.calls.lock().unwrap().len();
    engine.tick(false).await.unwrap();
    assert_eq!(runner.calls.lock().unwrap().len(), count);
}

#[tokio::test]
async fn unknown_fence_survives_observation_failures_and_only_current_stopped_confirmation() {
    let (_directory, engine, runner, clock) = fixture().await;
    save(&engine, "a", false).await;
    engine
        .update(
            json(r#"{"action":"run","operationId":"first-a","id":"a"}"#),
            None,
        )
        .await
        .unwrap();
    engine.tick(false).await.unwrap();
    wait(|| engine.active("a")).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if engine
                .store()
                .call(|s| s.run("first-a"))
                .await
                .unwrap()
                .unwrap()
                .get("workerJobName")
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // Model an unresolved persisted observation while the real engine still
    // owns an active runner: a caller assertion cannot override active work.
    engine
        .store()
        .call(|s| {
            let mut run = s.run("first-a")?.unwrap();
            run.insert("error", "outcome_unknown".into());
            s.save_run(&run)
        })
        .await
        .unwrap();
    let busy=engine.update(json(r#"{"action":"disable","operationId":"busy-confirm","id":"a","expectedRevision":1,"confirmedStoppedRunId":"first-a"}"#),None).await.unwrap_err();
    assert_eq!(busy.code, "task_busy");
    engine
        .store()
        .call(|s| {
            let mut run = s.run("first-a")?.unwrap();
            run.insert("error", IvyJson::Null);
            s.save_run(&run)
        })
        .await
        .unwrap();
    engine
        .update(
            json(r#"{"action":"disable","operationId":"stop-a","id":"a","expectedRevision":1}"#),
            None,
        )
        .await
        .unwrap();
    wait(|| !engine.active("a")).await;
    for (now, failure) in [(1003000, false), (1006000, true), (1009000, false)] {
        runner.observation_failure.store(failure, Ordering::Release);
        clock.0.store(now, Ordering::Release);
        engine.tick(false).await.unwrap();
        let run = engine
            .store()
            .call(|s| s.run("first-a"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(run.get("error"), Some(&"outcome_unknown".into()));
        assert_eq!(runner.calls.lock().unwrap().len(), 1);
    }
    engine
        .store()
        .call(|s| {
            let first = s.run("first-a")?.unwrap();
            let mut stale = first.clone();
            stale.insert("id", "stale-a".into());
            s.save_run(&stale)?;
            let mut foreign = first;
            foreign.insert("id", "foreign-b".into());
            foreign.insert("taskId", "b".into());
            let mut task = foreign.required("task")?.clone();
            task.insert("id", "b".into());
            foreign.insert("task", task);
            s.save_run(&foreign)
        })
        .await
        .unwrap();
    for (operation, confirmed, revision, code) in [
        ("stale-confirm", "stale-a", 2.0, "operation_conflict"),
        ("foreign-confirm", "foreign-b", 2.0, "operation_conflict"),
        ("missing-confirm", "missing", 2.0, "operation_conflict"),
        ("stale-revision", "first-a", 1.0, "revision_conflict"),
    ] {
        let error = engine
            .update(
                IvyJson::object([
                    ("action", "disable".into()),
                    ("operationId", operation.into()),
                    ("id", "a".into()),
                    ("expectedRevision", IvyJson::Number(revision)),
                    ("confirmedStoppedRunId", confirmed.into()),
                ]),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(
            engine
                .store()
                .call(|s| s.run("first-a"))
                .await
                .unwrap()
                .unwrap()
                .get("error"),
            Some(&"outcome_unknown".into())
        );
    }
    engine.update(json(r#"{"action":"disable","operationId":"matching-confirm","id":"a","expectedRevision":2,"confirmedStoppedRunId":"first-a"}"#),None).await.unwrap();
    let confirmed = engine
        .store()
        .call(|s| s.run("first-a"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        confirmed.get("error"),
        Some(&"Operator confirmed worker termination; execution outcome remains unknown.".into())
    );
    engine
        .update(
            json(r#"{"action":"run","operationId":"after-confirm","id":"a"}"#),
            None,
        )
        .await
        .unwrap();
    engine.tick(false).await.unwrap();
    wait(|| runner.calls.lock().unwrap().len() == 2).await;
    runner.finish.add_permits(1);
    wait(|| !engine.active("a")).await;
    assert_eq!(
        engine
            .store()
            .call(|s| s.run("after-confirm"))
            .await
            .unwrap()
            .unwrap()
            .get("status"),
        Some(&"publishing".into())
    );
    engine.close().await;
}


struct ControlPeer {
    assignment: Mutex<IvyJson>,
    definition: IvyJson,
    script: IvyJson,
    writes: std::sync::atomic::AtomicUsize,
    store: StoreOwner,
}
fn control_document(id: &str, kind: &str, revision: f64, value: IvyJson) -> IvyJson {
    IvyJson::object([
        ("object",IvyJson::object([("id",id.into()),("contractKey",format!("data-collector/{kind}").into()),("effectivelyArchived",false.into())])),
        ("revision",IvyJson::object([("revision",revision.into())])),
        ("content",IvyJson::object([("encoding","json".into()),("value",value)])),
    ])
}
impl ivy_sdk::rpc::RpcClient for ControlPeer {
    fn request(&self, method: &str, params: IvyJson, _: ivy_sdk::rpc::RequestOptions) -> ivy_sdk::rpc::RequestFuture<'_> {
        let method=method.to_owned();
        Box::pin(async move {Ok(match method.as_str() {
            "system.status"=>json(r#"{"runtimeEpoch":"00000000-0000-4000-8000-000000000000"}"#),
            "objects.query"=>{
                let items=match model::text(&params,"contractKey")?.as_str() {
                    "data-collector/root"=>vec![json(r#"{"objectId":"root"}"#)],
                    "data-collector/assignment"=>vec![IvyJson::object([("document",self.assignment.lock().unwrap().clone())])],
                    other=>panic!("Unexpected control query {other}"),
                };
                IvyJson::object([("items",IvyJson::Array(items)),("nextCursor",IvyJson::Null)])
            },
            "objects.read"=>match model::text(&params,"objectId")?.as_str() {
                "job"=>self.definition.clone(),"script"=>self.script.clone(),
                "assignment"=>self.assignment.lock().unwrap().clone(),
                other=>panic!("Unexpected control read {other}"),
            },
            "objects.write"=>{
                assert_eq!(params.get("objectId"),Some(&"assignment".into()));
                if params.required("content")?.required("value")?.get("enabled")==Some(&false.into()) {
                    // The scheduler must already see disabled before the remote write.
                    let row=self.store.call(|s|s.get("task",false)).await?.unwrap();
                    assert!(!model::truth(row.required("task")?,"enabled"));
                }
                let mut current=self.assignment.lock().unwrap();
                let revision=model::number(current.required("revision")?,"revision")?;
                assert_eq!(params.get("expectedRevision"),Some(&revision.into()));
                let mut updated=current.clone();updated.insert("revision",IvyJson::object([("revision",(revision+1.0).into())]));
                updated.insert("content",params.required("content")?.clone());*current=updated.clone();
                self.writes.fetch_add(1,Ordering::Relaxed);updated
            },
            other=>panic!("Unexpected control RPC {other}: {params:?}"),
        })})
    }
}

#[tokio::test]
async fn idle_activation_preserves_queued_running_and_publishing_work_and_original_receipts() {
    let (_directory,engine,runner,clock)=fixture().await;save(&engine,"task",true).await;
    let mut task=engine.store().call(|s|Ok(s.get("task",false)?.unwrap().required("task")?.clone())).await.unwrap();
    let script=task.remove("script").unwrap();task.remove("enabled");
    let peer=Arc::new(ControlPeer {
        assignment:Mutex::new(control_document("assignment","assignment",1.0,json(r#"{"schemaVersion":1,"jobId":"job","definitionRevision":1,"serviceNodeId":"fixture.collector","enabled":true,"deleted":false,"rootObjectId":"root","resultObjectId":null}"#))),
        definition:control_document("job","definition",1.0,IvyJson::object([("schemaVersion",1.0.into()),("homeServiceNodeId","fixture.collector".into()),("task",task),("script",json(r#"{"objectId":"script","revision":1}"#))])),
        script:control_document("script","script",1.0,IvyJson::object([("schemaVersion",1.0.into()),("script",script)])),
        writes:Default::default(),store:engine.store(),
    });
    engine.attach(peer.clone()).await.unwrap();
    engine.store().call(|s|{let mut row=s.get("task",false)?.unwrap();row.insert("definition",json(r#"{"jobId":"job","revision":1,"assignmentId":"assignment","assignmentRevision":1}"#));s.save(&row)}).await.unwrap();
    let control=ivy_data_collector::definitions::Definitions::new("fixture.collector".into(),engine.clone(),Arc::new(|_|Ok(None)));
    let input=json(r#"{"action":"activate","operationId":"pause","jobId":"job","definitionRevision":1,"expectedRevision":1,"enabled":false,"onlyWhenIdle":true}"#);
    let original=engine.store().call(|s|s.get("task",false)).await.unwrap();
    for (field,value,code) in [
        ("enabled",true.into(),"invalid_arguments"),
        ("serviceNodeId","another.collector".into(),"invalid_arguments"),
        ("definitionRevision",2.0.into(),"invalid_arguments"),
        ("expectedRevision",2.0.into(),"revision_conflict"),
    ] {
        let mut bad=input.clone();bad.insert("operationId",format!("bad-{field}").into());bad.insert(field,value);
        assert_eq!(control.update(bad,None).await.unwrap_err().code,code);
        assert_eq!(engine.store().call(|s|s.get("task",false)).await.unwrap(),original);
        assert_eq!(peer.writes.load(Ordering::Relaxed),0);
    }
    engine.update(json(r#"{"action":"run","operationId":"first","id":"task"}"#),None).await.unwrap();
    let queued=engine.store().call(|s|s.run("first")).await.unwrap();
    let queued_row=engine.store().call(|s|s.get("task",false)).await.unwrap();
    assert_eq!(control.update(input.clone(),None).await.unwrap_err().code,"task_busy");
    assert_eq!(engine.store().call(|s|s.run("first")).await.unwrap(),queued);
    assert_eq!(engine.store().call(|s|s.get("task",false)).await.unwrap(),queued_row);
    assert_eq!(peer.writes.load(Ordering::Relaxed),0);
    engine.tick(false).await.unwrap();wait(||runner.calls.lock().unwrap().len()==1).await;
    assert_eq!(control.update(input.clone(),None).await.unwrap_err().code,"task_busy");
    assert!(engine.active("task"));assert_eq!(peer.writes.load(Ordering::Relaxed),0);
    runner.finish.add_permits(1);wait(||!engine.active("task")).await;
    assert_eq!(engine.pending_publications().await.unwrap(),1);
    assert_eq!(control.update(input.clone(),None).await.unwrap_err().code,"task_busy");
    // Model the publisher's acknowledgement after the real worker completed.
    engine.store().call(|s|{let mut r=s.run("first")?.unwrap();r.insert("status","succeeded".into());s.save_run(&r)}).await.unwrap();
    let completed=engine.store().call(|s|s.run("first")).await.unwrap();
    let due_at=engine.store().call(|s|model::number(&s.get("task",false)?.unwrap(),"nextAt")).await.unwrap();
    let paused=control.update(input.clone(),None).await.unwrap();
    assert!(paused.required("adoptionError").unwrap().is_null());
    assert_eq!(peer.writes.load(Ordering::Relaxed),1);
    assert_eq!(engine.store().call(|s|s.run("first")).await.unwrap(),completed);
    assert_eq!(engine.store().call(|s|model::number(&s.get("task",false)?.unwrap(),"nextAt")).await.unwrap(),due_at);
    clock.0.store(2_000_000,Ordering::Release);engine.tick(false).await.unwrap();
    assert_eq!(runner.calls.lock().unwrap().len(),1);
    let mut resume=input.clone();resume.insert("operationId","resume".into());resume.insert("expectedRevision",2.0.into());resume.insert("enabled",true.into());resume.remove("onlyWhenIdle");
    control.update(resume,None).await.unwrap();
    assert_eq!(engine.store().call(|s|model::number(&s.get("task",false)?.unwrap(),"nextAt")).await.unwrap(),due_at);
    // Work that became due during maintenance runs on the next scheduler tick.
    engine.tick(false).await.unwrap();wait(||runner.calls.lock().unwrap().len()==2).await;
    let later_id=engine.store().call(|s|model::text(&s.get("task",false)?.unwrap(),"lastRunId")).await.unwrap();
    assert_eq!(control.update(input,None).await.unwrap(),paused);
    assert_eq!(peer.writes.load(Ordering::Relaxed),2);assert!(engine.active("task"));
    runner.finish.add_permits(1);wait(||!engine.active("task")).await;
    let later=engine.store().call(move |s|s.run(&later_id)).await.unwrap().unwrap();
    assert_eq!(later.get("status"),Some(&"publishing".into()));assert!(later.required("error").unwrap().is_null());
    engine.close().await;
}

#[tokio::test]
async fn idle_disable_checks_active_owner_gap_and_unknown_outcome_without_cancellation() {
    let (_directory,engine,_runner,_clock)=fixture().await;save(&engine,"task",true).await;
    let before=engine.store().call(|s|s.get("task",false)).await.unwrap();
    let settings=engine.settings().clone();
    let busy=engine.store().call(move |s|ivy_data_collector::runtime::Runtime{store:s,settings:&settings,root_id:"root"}
        .update(&json(r#"{"action":"disable","operationId":"active-gap","id":"task","expectedRevision":1,"onlyWhenIdle":true}"#),None,1000000,"1970-01-01T00:16:40.000Z",|_|true,|_|Ok(()))).await.err().unwrap();
    assert_eq!(busy.code,"task_busy");assert_eq!(engine.store().call(|s|s.get("task",false)).await.unwrap(),before);
    engine.store().call(|s|{
        let mut row=s.get("task",false)?.unwrap();row.insert("lastRunId","unknown".into());s.save(&row)?;
        s.save_run(&json(r#"{"id":"unknown","taskId":"task","task":{},"status":"failed","error":"outcome_unknown","finishedAt":"1970-01-01T00:16:40.000Z"}"#))
    }).await.unwrap();
    let prior=engine.store().call(|s|s.run("unknown")).await.unwrap();
    assert_eq!(engine.update(json(r#"{"action":"disable","operationId":"unknown-idle","id":"task","expectedRevision":1,"onlyWhenIdle":true}"#),None).await.unwrap_err().code,"outcome_unknown");
    assert_eq!(engine.store().call(|s|s.run("unknown")).await.unwrap(),prior);
    engine.close().await;
}
