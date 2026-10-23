//! Bounded diagnostic state, independent of authoritative deployment outcomes.
//! Output contents never enter the progress file. A queued write retains its
//! ownership when an individual step/close waiter is dropped.
use crate::{IvyError, IvyJson, IvyResult, config, native_path, process::OutputSink};
use ivy_contracts::{JsString, hash::hash_json};
use ivy_sdk::clock::{Clock, SystemClock, parse_date};
use std::{path::PathBuf, sync::{Arc, Mutex}, time::Duration};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

fn now(clock: &dyn Clock) -> IvyResult<IvyJson> {
    chrono::DateTime::from_timestamp_millis(clock.utc_millis()).map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true).into()).ok_or_else(IvyError::internal)
}
fn location(config: &IvyJson, component: &IvyJson, snapshot: &IvyJson) -> IvyResult<PathBuf> {
    let hash = hash_json(&IvyJson::object([("componentId",component.clone()),("snapshotId",snapshot.clone())]))?;
    let root = config::path_value(config.required("runtimeRoot")?)?;
    Ok(root.join("preparation-progress").join(format!("{}.json",&hash[7..])))
}
struct Write { value: IvyJson, finished: oneshot::Sender<()> }
struct State { value: Mutex<IvyJson>, clock: Arc<dyn Clock> }
impl State {
    fn snapshot(&self) -> IvyResult<IvyJson> {
        let mut value=self.value.lock().map_err(|_|IvyError::internal())?.clone();value.insert("observedAt",now(self.clock.as_ref())?);Ok(value)
    }
    fn output(&self, bytes: usize) {
        let Ok(at)=now(self.clock.as_ref()) else{return};
        if let Ok(mut value)=self.value.lock(){
            let total=value.get("outputBytes").and_then(IvyJson::as_f64).unwrap_or(0.0)+bytes as f64;
            value.insert("lastOutputAt",at);value.insert("outputBytes",IvyJson::Number(total));
        }
    }
}
pub struct PreparationProgress { state: Arc<State>, sender: mpsc::UnboundedSender<Write>, stop: CancellationToken }
#[derive(Default)]
pub struct StepOptions { pub command: Option<JsString>, pub file_path: Option<JsString> }
impl PreparationProgress {
    pub fn new(config: &IvyJson, component_id: JsString, snapshot_id: JsString) -> IvyResult<Self> {
        Self::with_clock(config,component_id,snapshot_id,Arc::new(SystemClock))
    }
    pub fn with_clock(config: &IvyJson, component_id: JsString, snapshot_id: JsString, clock: Arc<dyn Clock>) -> IvyResult<Self> {
        let component=IvyJson::String(component_id);let snapshot=IvyJson::String(snapshot_id);
        let path=location(config,&component,&snapshot)?;let at=now(clock.as_ref())?;
        let state=Arc::new(State{clock,value:Mutex::new(IvyJson::object([
            ("componentId",component),("snapshotId",snapshot),("step","starting".into()),("startedAt",at.clone()),("stepStartedAt",at.clone()),("observedAt",at),
            ("lastOutputAt",IvyJson::Null),("outputBytes",IvyJson::Number(0.0)),("workerPid",IvyJson::Number(std::process::id() as f64)),
            ("command",IvyJson::Null),("filePath",IvyJson::Null),("fileBytes",IvyJson::Null),
        ]))});
        let (sender,mut queue)=mpsc::unbounded_channel::<Write>();
        let runtime=tokio::runtime::Handle::try_current().map_err(|_|IvyError::internal())?;
        runtime.spawn(async move {
            while let Some(mut request)=queue.recv().await {
                if let Some(file)=request.value.get("filePath").and_then(IvyJson::as_js_string).filter(|s|!s.0.is_empty()) {
                    let file=native_path::to_native(file);
                    let bytes=tokio::task::spawn_blocking(move ||std::fs::metadata(file).ok().map(|m|m.len())).await.ok().flatten();
                    request.value.insert("fileBytes",bytes.map_or(IvyJson::Null,|n|IvyJson::Number(n as f64)));
                }
                let _=config::atomic_json(path.clone(),request.value).await;
                let _=request.finished.send(());
            }
        });
        let stop=CancellationToken::new();let timer_stop=stop.clone();let timer_state=state.clone();let timer_sender=sender.clone();
        runtime.spawn(async move {
            let mut timer=tokio::time::interval_at(tokio::time::Instant::now()+Duration::from_secs(5),Duration::from_secs(5));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {tokio::select!{biased;_ = timer_stop.cancelled()=>break,_ = timer.tick()=>{
                if let Ok(value)=timer_state.snapshot(){let(finished,_)=oneshot::channel();if timer_sender.send(Write{value,finished}).is_err(){break;}}
            }}}
        });
        Ok(Self{state,sender,stop})
    }
    async fn publish(&self) -> IvyResult<()> {
        let (finished,wait)=oneshot::channel();
        self.sender.send(Write{value:self.state.snapshot()?,finished}).map_err(|_|IvyError::internal())?;
        let _=wait.await;Ok(())
    }
    pub async fn step(&self, step: JsString, options: StepOptions) -> IvyResult<()> {
        {let mut value=self.state.value.lock().map_err(|_|IvyError::internal())?;
            value.insert("step",IvyJson::String(step));value.insert("stepStartedAt",now(self.state.clock.as_ref())?);
            for key in ["lastOutputAt","fileBytes"]{value.insert(key,IvyJson::Null);}value.insert("outputBytes",IvyJson::Number(0.0));
            value.insert("command",options.command.map_or(IvyJson::Null,IvyJson::String));value.insert("filePath",options.file_path.map_or(IvyJson::Null,IvyJson::String));
        }
        self.publish().await
    }
    pub fn output(&self,text:&JsString){self.state.output(String::from_utf16_lossy(&text.0).len());}
    pub fn output_sink(&self)->OutputSink {
        let state=self.state.clone();Arc::new(move |_,text|state.output(text.len()))
    }
    pub async fn close(&self)->IvyResult<()> {self.stop.cancel();self.publish().await}
}
impl Drop for PreparationProgress {fn drop(&mut self){self.stop.cancel();}}

fn date(value:Option<&IvyJson>)->IvyResult<f64> {Ok(value.and_then(IvyJson::as_js_string).map_or(f64::NAN,parse_date))}
pub async fn preparation_progress(config:&IvyJson,entry:&IvyJson)->IvyResult<Option<IvyJson>> {
    preparation_progress_with_clock(config,entry,&SystemClock).await
}
pub async fn preparation_progress_with_clock(config:&IvyJson,entry:&IvyJson,clock:&dyn Clock)->IvyResult<Option<IvyJson>> {
    let Some(snapshot)=entry.get("sourceSnapshot").filter(|v|!v.is_null()) else{return Ok(None)};
    let record=entry.required("record")?;
    if record.get("phase")!=Some(&"preparing".into()){return Ok(None)}
    let component=record.required("componentId")?;let snapshot_id=snapshot.required("snapshotId")?;
    let value=match config::json_file(location(config,component,snapshot_id)?,16384).await {Ok(value)=>value,Err(_)=>return Ok(None)};
    if value.get("componentId")!=Some(component)||value.get("snapshotId")!=Some(snapshot_id){return Ok(None)}
    let started=date(value.get("startedAt"))?;let step=date(value.get("stepStartedAt"))?;let observed=date(value.get("observedAt"))?;
    if !started.is_finite()||!step.is_finite()||!observed.is_finite()||started<date(record.get("createdAt"))? {return Ok(None)}
    let mut result=value;
    result.insert("elapsedMs",IvyJson::Number((clock.utc_millis() as f64-started).max(0.0)));
    result.insert("stepElapsedMs",IvyJson::Number((clock.utc_millis() as f64-step).max(0.0)));
    result.insert("heartbeatAgeMs",IvyJson::Number((clock.utc_millis() as f64-observed).max(0.0)));
    Ok(Some(result))
}
