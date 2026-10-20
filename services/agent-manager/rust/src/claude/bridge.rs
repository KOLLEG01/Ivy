//! Private, versioned transport to the pinned official SDK. This layer owns
//! only child/pipe/call lifetimes; SDK and native policy stay in their owners.
use super::{
    error::{AdapterError, Result},
    value::{JsonScalar, decode, encode, field},
};
use ivy_contracts::IvyJson as J;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{mpsc, oneshot},
};

pub const VERSION: f64 = 1.0;
const MAXIMUM_BYTES: usize = 32 * 1024 * 1024;
pub struct BridgeLaunch {
    pub node_executable: PathBuf,
    pub entry: PathBuf,
    pub cwd: PathBuf,
    /// Complete, already admitted environment; no ambient credential fallback.
    pub environment: BTreeMap<OsString, OsString>,
}
enum CommandMessage {
    Frame(Vec<u8>),
}
type Pending = Arc<Mutex<BTreeMap<u64, oneshot::Sender<Result<J>>>>>;
#[derive(Clone)]
pub struct Bridge {
    sender: mpsc::Sender<CommandMessage>,
    next: Arc<AtomicU64>,
    pending: Pending,
    stops: mpsc::UnboundedSender<oneshot::Sender<()>>,
    closed: Arc<AtomicBool>,
}
impl Bridge {
    pub fn spawn(
        launch: BridgeLaunch,
        diagnostic: Arc<dyn Fn(&[u8]) + Send + Sync>,
    ) -> Result<(Self, mpsc::Receiver<J>)> {
        let mut command = Command::new(&launch.node_executable);
        command
            .arg(&launch.entry)
            .current_dir(&launch.cwd)
            .env_clear()
            .envs(&launch.environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let mut child = command.spawn()?;
        let mut input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        let mut errors = child.stderr.take().unwrap();
        let (sender, mut commands) = mpsc::channel(64);
        let (stops, mut stop_receiver) = mpsc::unbounded_channel::<oneshot::Sender<()>>();
        let (events, event_receiver) = mpsc::channel(128);
        let pending: Pending = Arc::new(Mutex::new(BTreeMap::new()));
        let waiting = pending.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let exited = closed.clone();
        let diagnostics = tokio::spawn(async move {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = errors.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                diagnostic(&chunk[..n]);
            }
        });
        tokio::spawn(async move {
            let mut chunk = [0u8; 8192];
            let mut buffer = Vec::new();
            let mut stop_reply = None;
            let failure: AdapterError = 'owner: loop {
                tokio::select! {
                    biased;
                    stop=stop_receiver.recv()=>{stop_reply=stop;break AdapterError::new("SDK bridge stopped")},
                    message=commands.recv()=>match message{
                        Some(CommandMessage::Frame(frame))=>{
                            tokio::select!{
                                stop=stop_receiver.recv()=>{stop_reply=stop;break 'owner AdapterError::new("SDK bridge stopped")},
                                write=input.write_all(&frame)=>if let Err(error)=write{break 'owner error.into()},
                            }
                        },
                        None=>break AdapterError::new("SDK bridge owner closed"),
                    },
                    read=output.read(&mut chunk)=>{
                        let count=match read{Ok(0)=>break AdapterError::new("SDK bridge output closed"),Ok(n)=>n,Err(e)=>break e.into()};
                        buffer.extend_from_slice(&chunk[..count]);
                        let mut consumed=0;
                        let mut error=None;
                        while let Some(relative)=buffer[consumed..].iter().position(|b|*b==b'\n'){
                            let end=consumed+relative;
                            if end-consumed>MAXIMUM_BYTES{error=Some(AdapterError::new("SDK bridge frame exceeds byte limit"));break}
                            let frame=match decode(&buffer[consumed..end]){Ok(frame)=>frame,Err(e)=>{error=Some(e);break}};consumed=end+1;
                            if field(&frame,"version").as_f64()!=Some(VERSION){error=Some(AdapterError::new("SDK bridge version mismatch"));break}
                            if let Some(id)=field(&frame,"id").as_js_string().and_then(|s|s.to_utf8().ok()).and_then(|s|s.parse::<u64>().ok()){
                                let reply=waiting.lock().unwrap().remove(&id);
                                if let Some(reply)=reply{
                                    let result=if !field(&frame,"error").is_null(){Err(AdapterError::new(super::value::string(field(field(&frame,"error"),"message"))))}else{Ok(field(&frame,"result").clone())};
                                    let _=reply.send(result);
                                }
                            }else{
                                tokio::select!{
                                    stop=stop_receiver.recv()=>{stop_reply=stop;break 'owner AdapterError::new("SDK bridge stopped")},
                                    ready=events.reserve()=>match ready{Ok(permit)=>permit.send(frame),Err(_)=>{error=Some(AdapterError::new("SDK bridge event owner closed"));break}},
                                    exit=child.wait()=>break 'owner AdapterError::new(format!("SDK bridge exited: {}",exit.map(|x|x.to_string()).unwrap_or_else(|e|e.to_string()))),
                                }
                            }
                        }
                        if let Some(error)=error{break error}
                        if consumed>0{buffer.drain(..consumed);}
                        if buffer.len()>MAXIMUM_BYTES{break AdapterError::new("SDK bridge frame exceeds byte limit")}
                    },
                    exit=child.wait()=>break AdapterError::new(format!("SDK bridge exited: {}",exit.map(|x|x.to_string()).unwrap_or_else(|e|e.to_string()))),
                }
            };
            exited.store(true, Ordering::Release);
            // This is the exact owned child, never a process-name or host scan.
            let _ = input.shutdown().await;
            let _ = child.start_kill();
            let _ = child.wait().await;
            diagnostics.abort();
            let _ = diagnostics.await;
            for (_, reply) in std::mem::take(&mut *waiting.lock().unwrap()) {
                let _ = reply.send(Err(AdapterError::new(failure.message.clone())));
            }
            if let Some(reply) = stop_reply {
                let _ = reply.send(());
            } else {
                let _ = events.try_send(J::object([
                    ("version", VERSION.into_json()),
                    ("type", "transportError".into()),
                    ("message", J::String(failure.message)),
                ]));
            }
        });
        Ok((
            Self {
                sender,
                next: Arc::new(AtomicU64::new(1)),
                pending,
                stops,
                closed,
            },
            event_receiver,
        ))
    }
    pub async fn call(&self, method: &str, params: J) -> Result<J> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let mut frame = encode(&J::object([
            ("version", VERSION.into_json()),
            ("id", id.to_string().into()),
            ("method", method.into()),
            ("params", params),
        ]))?
        .into_bytes();
        if frame.len() > MAXIMUM_BYTES {
            return Err(AdapterError::new("SDK bridge frame exceeds byte limit"));
        }
        frame.push(b'\n');
        {
            let mut pending = self.pending.lock().unwrap();
            if self.closed.load(Ordering::Acquire) {
                return Err(AdapterError::new("SDK bridge is closed"));
            }
            pending.insert(id, tx);
        }
        // Cancellation removes only this waiter; a written SDK command is
        // never replayed because its caller dropped the response future.
        struct Guard {
            pending: Pending,
            id: u64,
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                self.pending.lock().unwrap().remove(&self.id);
            }
        }
        let _guard = Guard {
            pending: self.pending.clone(),
            id,
        };
        self.sender
            .send(CommandMessage::Frame(frame))
            .await
            .map_err(|_| AdapterError::new("SDK bridge is closed"))?;
        rx.await
            .map_err(|_| AdapterError::new("SDK bridge response owner closed"))?
    }
    pub async fn stop(&self) -> Result<()> {
        // Do not wait on provider/control promises to establish child cleanup.
        let (tx, rx) = oneshot::channel();
        if self.stops.send(tx).is_err() {
            return Ok(());
        }
        let _ = rx.await;
        Ok(())
    }
}
