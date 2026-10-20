use ivy_contracts::{IvyJson,json::{parse,stringify}};
use ivy_phone_bridge::{dtmf::InputSource,media::{AudioDesktopIdentity,AudioNegotiator,AudioRoutePermit,AudioSettings,
    AudioSendHealth,CallAudioError,CallAudioFactory,CallAudioPort,CallAudioRoute,CallWaitingAudio,Codec,
    PcmAudioPort,PcmAudioState,PcmSendPort,RtpPacket,RtpPeer,SdpDocument,SdpType,ScreeningAudio,
    SipReceiveEvents,SipRtpReceiver,load_announcement,parse_announcement}};
use std::{collections::VecDeque,future::Future,io::Write,path::{Path,PathBuf},pin::Pin,process::{Command,Stdio},
    sync::{Arc,Condvar,Mutex,atomic::{AtomicBool,AtomicUsize,Ordering}},time::Duration};
use tokio::{net::UdpSocket,sync::Notify,time::{sleep,timeout}};
use tokio_util::sync::CancellationToken;
const CALL:&str="11111111-1111-4111-8111-111111111111";
fn n(value:usize)->IvyJson{IvyJson::Number(value as f64)}
fn hex(bytes:&[u8])->String{use std::fmt::Write;bytes.iter().fold(String::new(),|mut s,b|{write!(s,"{b:02x}").unwrap();s})}
fn floats(values:&[f32])->String{hex(&values.iter().flat_map(|v|v.to_le_bytes()).collect::<Vec<_>>())}
fn trace(name:&str,value:IvyJson){if let Some(root)=std::env::var_os("IVY_PHONE_CALL_AUDIO_TRACE_DIR"){std::fs::write(PathBuf::from(root).join(format!("{name}.json")),stringify(&value,16_000_000).unwrap()).unwrap();}}
fn original(inputs:&[IvyJson])->Option<IvyJson>{
    if std::env::var_os("IVY_PHONE_CONFIG_ORACLE_DIR").is_none(){eprintln!("Original call audio oracle unavailable; differential not executed");return None;}
    let mut child=Command::new("pwsh").args(["-NoProfile","-File","tests/fixtures/phone-call-audio-oracle.ps1"])
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"),"/../../..")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(stringify(&IvyJson::Array(inputs.to_vec()),16_000_000).unwrap().as_bytes()).unwrap();
    let out=child.wait_with_output().unwrap();assert!(out.status.success(),"{}",String::from_utf8_lossy(&out.stderr));
    Some(parse(std::str::from_utf8(&out.stdout).unwrap().trim_start_matches('\u{feff}'),32_000_000).unwrap())
}
#[derive(Default)]
struct Gate{entered:AtomicBool,released:AtomicBool,changed:Notify}
impl Gate{async fn block(&self){self.entered.store(true,Ordering::SeqCst);loop{let n=self.changed.notified();tokio::pin!(n);n.as_mut().enable();if self.released.load(Ordering::SeqCst){break;}n.await;}}fn release(&self){self.released.store(true,Ordering::SeqCst);self.changed.notify_waiters();}async fn wait(&self){wait(||self.entered.load(Ordering::SeqCst)).await;}}
#[derive(Default)]
struct CaptureGate{entered:AtomicBool,released:Mutex<bool>,changed:Condvar}
impl CaptureGate{fn block(&self){self.entered.store(true,Ordering::SeqCst);let g=self.released.lock().unwrap();let(g,_)=self.changed.wait_timeout_while(g,Duration::from_secs(5),|v|!*v).unwrap();assert!(*g);}fn release(&self){*self.released.lock().unwrap()=true;self.changed.notify_all();}}
struct RouteState{mode:&'static str,generation:u64,permit:Option<AudioRoutePermit>,resumes:usize,suspends:usize,disposals:usize,reads:usize,writes:usize,bad_close:bool,bad_suspend:bool}
struct Route{state:Mutex<RouteState>,close_gate:Option<Arc<Gate>>,read_gate:Mutex<Option<Arc<CaptureGate>>>,flip_snapshot:AtomicBool}
impl Route {
    fn new()->Self{Self{state:Mutex::new(RouteState{mode:"suspended",generation:0,permit:None,resumes:0,suspends:0,disposals:0,reads:0,writes:0,bad_close:false,bad_suspend:false}),close_gate:None,read_gate:Mutex::new(None),flip_snapshot:AtomicBool::new(false)}}
    fn pcm(state:&RouteState)->PcmAudioState{PcmAudioState{open:state.mode=="open"&&state.permit.as_ref().is_some_and(AudioRoutePermit::permitted),generation:state.generation}}
    fn mode(&self,mode:&'static str){self.state.lock().unwrap().mode=mode;}
}
impl PcmAudioPort for Route {
    fn state(&self)->PcmAudioState{let mut state=self.state.lock().unwrap();let snapshot=Self::pcm(&state);if self.flip_snapshot.swap(false,Ordering::SeqCst){state.generation+=1;state.mode="suspended";}snapshot}
    fn read_captured(&self,out:&mut[f32],generation:u64)->usize{let current={let mut state=self.state.lock().unwrap();state.reads+=1;let pcm=Self::pcm(&state);pcm.open&&pcm.generation==generation};out.fill(if current{0.25}else{0.0});let gate=self.read_gate.lock().unwrap().take();if let Some(gate)=gate{gate.block();}if current{out.len()}else{0}}
    fn write_received(&self,_:&[f32],generation:u64)->bool{let mut state=self.state.lock().unwrap();let pcm=Self::pcm(&state);if pcm.open&&pcm.generation==generation{state.writes+=1;true}else{false}}
}
impl PcmSendPort for Route {
    fn with_send_state(&self,admitted:PcmAudioState,action:&mut dyn FnMut())->bool{let state=self.state.lock().unwrap();if Self::pcm(&state)!=admitted{return false;}action();drop(state);true}
    fn send_health(&self)->AudioSendHealth{if self.failed(){AudioSendHealth::Failed}else{AudioSendHealth::Ready}}
}
impl CallAudioRoute for Route {
    fn status(&self)->IvyJson{IvyJson::object([("state",self.state.lock().unwrap().mode.into())])}
    fn failed(&self)->bool{self.state.lock().unwrap().mode=="failed"}
    fn resume(&self)->Result<(),CallAudioError>{let mut state=self.state.lock().unwrap();if !state.permit.as_ref().is_some_and(AudioRoutePermit::permitted)||matches!(state.mode,"failed"|"closed"){return Err(CallAudioError::RouteFailed);}state.mode="open";state.generation+=1;state.resumes+=1;Ok(())}
    fn suspend(&self)->Result<(),CallAudioError>{let mut state=self.state.lock().unwrap();state.suspends+=1;if state.bad_suspend{return Err(CallAudioError::RouteFailed);}if state.mode=="open"{state.mode="suspended";}Ok(())}
    fn close(&self)->Pin<Box<dyn Future<Output=Result<(),CallAudioError>>+Send+'_>>{Box::pin(async move{{self.state.lock().unwrap().disposals+=1;}if let Some(gate)=&self.close_gate{gate.block().await;}let mut state=self.state.lock().unwrap();state.mode="closed";if state.bad_close{Err(CallAudioError::RouteFailed)}else{Ok(())}})}
}
struct Factory{routes:Mutex<VecDeque<Result<Arc<Route>,CallAudioError>>>,gate:Option<Arc<Gate>>,opens:AtomicUsize}
impl Factory{fn routes(routes:Vec<Arc<Route>>)->Self{Self{routes:Mutex::new(routes.into_iter().map(Ok).collect()),gate:None,opens:AtomicUsize::new(0)}}}
impl CallAudioFactory for Factory{
    fn open<'a>(&'a self,settings:&'a AudioSettings,_:Option<&'a AudioDesktopIdentity>,permit:AudioRoutePermit,_:CancellationToken)->Pin<Box<dyn Future<Output=Result<Arc<dyn CallAudioRoute>,CallAudioError>>+Send+'a>>{
        Box::pin(async move{assert_eq!(settings.capture_endpoint,"capture-fixture");assert!(!permit.permitted());self.opens.fetch_add(1,Ordering::SeqCst);
            if let Some(gate)=&self.gate{gate.block().await;}let route=self.routes.lock().unwrap().pop_front().unwrap()?;
            route.state.lock().unwrap().permit=Some(permit);Ok(route as Arc<dyn CallAudioRoute>)})
    }
}
fn settings()->AudioSettings{AudioSettings{capture_endpoint:"capture-fixture".into(),render_endpoint:"render-fixture".into(),source_mode:"microphone".into(),capture_buffer_ms:20,render_latency_ms:10,queue_ms:60,mute:None,playback_prebuffer_ms:20,enable_wasapi_low_latency:true,loopback_render_endpoint:None}}
fn desktop()->AudioDesktopIdentity{AudioDesktopIdentity::new(123,1,&std::env::temp_dir().join("fixture.exe"),"Fixture.Package!UI").unwrap()}
async fn wait(mut ready:impl FnMut()->bool){timeout(Duration::from_secs(5),async{while !ready(){sleep(Duration::from_millis(1)).await;}}).await.unwrap();}
fn snapshot(port:&CallAudioPort,route:&Route)->IvyJson{let status=port.status();let open=port.state().open;let failed=port.send_health()!=AudioSendHealth::Ready;let r=route.state.lock().unwrap();IvyJson::object([("state",status.state.into()),("open",open.into()),("failed",failed.into()),("route",r.mode.into()),("resumes",n(r.resumes)),("suspends",n(r.suspends)),("disposals",n(r.disposals)),("reads",n(r.reads)),("writes",n(r.writes))])}

#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn call_authority_lifetime_and_failed_cleanup_match_the_original_controller(){
    let sequences:Vec<Vec<&str>>=vec![vec!["prepare","activate","bind","grant","capture","write","suspend","capture","activate","renew","expire","capture","activate","renew","clear","capture","grant","release","bind","grant","revoke","capture","write","activate","prepare","close","close"],
        vec!["prepare","bind","grant","fail","capture","provider-open","grant","revoke","close"],
        vec!["prepare","bad-close","close","close","capture","activate"],vec!["prepare","bad-suspend","revoke","close","close"]];
    let inputs:Vec<_>=sequences.iter().map(|ops|IvyJson::object([("op","lifetime".into()),("actions",IvyJson::Array(ops.iter().map(|s|(*s).into()).collect()))])).collect();
    let Some(old)=original(&inputs)else{return;};let mut traces=Vec::new();
    for(ops,expected)in sequences.iter().zip(old.as_array().unwrap()){
        assert_eq!(expected.get("ok").unwrap().as_bool(),Some(true),"{expected:?}");
        let route=Arc::new(Route::new());let factory=Arc::new(Factory::routes(vec![route.clone()]));let port=CallAudioPort::new(CALL.into(),factory.clone()).unwrap();let mut rows=Vec::new();
        for name in ops{let result=match *name{
            "prepare"=>match port.prepare(settings(),None,CancellationToken::new()){Ok(op)=>op.wait().await,Err(e)=>Err(e)},
            "bind"=>port.bind_authority("capture-fixture"),"grant"=>port.grant_authority(),"renew"=>port.refresh_authority(port.now()),
            "expire"=>{sleep(Duration::from_millis(501)).await;Ok(())},"activate"=>port.activate(),"suspend"=>port.suspend(),"clear"=>port.clear_authority(),
            "release"=>port.release_voice_authority(),"arm"=>port.arm_voice_recovery(),"fail"=>{route.mode("failed");Ok(())},"provider-open"=>{route.mode("open");Ok(())},
            "capture"=>{port.read_captured(&mut[0.0;32],port.state().generation);Ok(())},"write"=>{port.write_received(&[0.0;32],port.state().generation);Ok(())},
            "bad-close"=>{route.state.lock().unwrap().bad_close=true;Ok(())},"bad-suspend"=>{route.state.lock().unwrap().bad_suspend=true;Ok(())},
            "revoke"=>{port.revoke();Ok(())},"close"=>port.close().wait().await,_=>unreachable!()};
            rows.push(IvyJson::object([("action",(*name).into()),("ok",result.is_ok().into()),("snapshot",snapshot(&port,&route))]));
        }
        assert_eq!(IvyJson::Array(rows.clone()),*expected.get("trace").unwrap(),"actions={ops:?}");let _=port.close().wait().await;traces.push(IvyJson::Array(rows));
    }
    trace("original-lifetime",IvyJson::object([("inputs",IvyJson::Array(inputs)),("original",old),("rust",IvyJson::Array(traces))]));
}

#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn late_preparation_waiters_and_cleanup_uncertainty_keep_the_original_fence(){
    for caller_cancel in [true,false]{let route=Arc::new(Route::new());let gate=Arc::new(Gate::default());let mut factory=Factory::routes(vec![route.clone()]);factory.gate=Some(gate.clone());let factory=Arc::new(factory);
        let port=CallAudioPort::new(CALL.into(),factory.clone()).unwrap();let cancel=CancellationToken::new();let prepare=port.prepare(settings(),None,cancel.clone()).unwrap();gate.wait().await;
        let waiter=tokio::spawn({let prepare=prepare.clone();async move{prepare.wait().await}});waiter.abort();assert!(waiter.await.unwrap_err().is_cancelled());
        if caller_cancel{cancel.cancel();}else{port.revoke();}let close=port.close();assert!(!close.finished());assert!(!port.state().open);
        gate.release();assert_eq!(prepare.wait().await,Err(CallAudioError::Cancelled));close.wait().await.unwrap();assert_eq!(factory.opens.load(Ordering::SeqCst),1);
        {let state=route.state.lock().unwrap();assert_eq!((state.disposals,state.resumes),(1,0));}assert_eq!(port.status().state,"closed");
    }
    for error in [CallAudioError::PreparationReleased,CallAudioError::CleanupUnconfirmed]{let factory=Factory{routes:Mutex::new(VecDeque::from([Err(error)])),gate:None,opens:AtomicUsize::new(0)};
        let port=CallAudioPort::new(CALL.into(),Arc::new(factory)).unwrap();assert_eq!(port.prepare(settings(),None,CancellationToken::new()).unwrap().wait().await,Err(error));
        let closed=port.close().wait().await;assert_eq!(closed.is_ok(),error==CallAudioError::PreparationReleased);assert_eq!(port.status().state,if closed.is_ok(){"closed"}else{"failed"});
    }
    trace("retained-preparation",IvyJson::object([("latePreparationsDisposedOnce",n(2)),("releasedFactoryAllowed",true.into()),("unknownFactoryFenced",true.into())]));
}

#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn one_same_owner_repair_waits_for_old_route_and_cannot_adopt_a_foreign_or_revoked_target(){
    let gate=Arc::new(Gate::default());let mut first=Route::new();first.close_gate=Some(gate.clone());let first=Arc::new(first);let second=Arc::new(Route::new());
    let factory=Arc::new(Factory::routes(vec![first.clone(),second.clone()]));let port=CallAudioPort::new(CALL.into(),factory.clone()).unwrap();let mut settings=settings();settings.source_mode="desktop_process".into();
    port.prepare(settings,Some(desktop()),CancellationToken::new()).unwrap().wait().await.unwrap();port.bind_authority("capture-fixture").unwrap();port.grant_authority().unwrap();port.arm_voice_recovery().unwrap();
    let old=port.state();first.mode("failed");assert_eq!(port.send_health(),AudioSendHealth::RepairPending);
    let foreign=AudioDesktopIdentity::new(124,1,desktop().image(),desktop().application()).unwrap();assert!(port.rebind(foreign,Arc::new(||true)).is_err());
    let repair=port.rebind(desktop(),Arc::new(||true)).unwrap();gate.wait().await;assert!(!port.state().open);assert_eq!(factory.opens.load(Ordering::SeqCst),1);gate.release();repair.wait().await.unwrap();
    let current=port.state();assert!(current.open&&current.generation>old.generation);let mut sent=false;assert!(!port.with_send_state(old,&mut||sent=true));assert!(!sent);
    second.mode("failed");assert_eq!(port.send_health(),AudioSendHealth::Failed);assert!(port.rebind(desktop(),Arc::new(||true)).is_err());port.close().wait().await.unwrap();
    assert_eq!(first.state.lock().unwrap().disposals,1);assert_eq!(second.state.lock().unwrap().disposals,1);
    trace("same-owner-repair",IvyJson::object([("opens",n(2)),("oldDisposedFirst",true.into()),("oldTokenRejected",true.into()),("secondRepairRejected",true.into())]));
}

#[tokio::test(flavor="multi_thread",worker_threads=3)]
async fn revoke_during_capture_clears_pcm_and_final_guard_never_recaptures_a_new_route_generation(){
    let route=Arc::new(Route::new());let factory=Arc::new(Factory::routes(vec![route.clone()]));let port=CallAudioPort::new(CALL.into(),factory).unwrap();port.prepare(settings(),None,CancellationToken::new()).unwrap().wait().await.unwrap();port.grant_authority().unwrap();
    let token=port.state();route.flip_snapshot.store(true,Ordering::SeqCst);let mut dispatch=false;assert!(!port.with_send_state(token,&mut||dispatch=true));assert!(!dispatch);
    port.activate().unwrap();let token=port.state();let read=Arc::new(CaptureGate::default());*route.read_gate.lock().unwrap()=Some(read.clone());
    let capture=tokio::task::spawn_blocking({let port=port.clone();move||{let mut pcm=[0.0;960];let count=port.read_captured(&mut pcm,token.generation);(count,pcm)}});
    wait(||read.entered.load(Ordering::SeqCst)).await;
    let revoked=Arc::new(AtomicBool::new(false));let revoke=tokio::task::spawn_blocking({let port=port.clone();let revoked=revoked.clone();move||{port.revoke();revoked.store(true,Ordering::SeqCst);}});
    sleep(Duration::from_millis(20)).await;assert!(!revoked.load(Ordering::SeqCst));read.release();
    let(count,pcm)=capture.await.unwrap();assert_eq!(count,0);assert!(pcm.iter().all(|s|*s==0.0));revoke.await.unwrap();port.close().wait().await.unwrap();
    trace("capture-final-guard",IvyJson::object([("obsoleteDispatches",n(0)),("postRevokePcmSamples",n(0)),("cleanupConfirmed",true.into())]));
}

fn wav(rate:u32,channels:u16,frames:usize)->Vec<u8>{let mut out=b"RIFF".to_vec();out.extend_from_slice(&0u32.to_le_bytes());out.extend_from_slice(b"WAVEfmt ");out.extend_from_slice(&16u32.to_le_bytes());out.extend_from_slice(&1u16.to_le_bytes());out.extend_from_slice(&channels.to_le_bytes());out.extend_from_slice(&rate.to_le_bytes());out.extend_from_slice(&(rate*u32::from(channels)*2).to_le_bytes());out.extend_from_slice(&(channels*2).to_le_bytes());out.extend_from_slice(&16u16.to_le_bytes());out.extend_from_slice(b"data");out.extend_from_slice(&((frames*channels as usize*2)as u32).to_le_bytes());for i in 0..frames*channels as usize{out.extend_from_slice(&((i as i16).wrapping_mul(7919)).to_le_bytes());}out}
#[test]
fn announcement_layout_and_linear_mix_match_the_original_including_chunk_edge_cases(){
    let mut corpus=Vec::new();for(rate,channels)in[(8000,1),(11025,2),(16000,3),(44100,8),(48000,1),(96000,2),(192000,8)]{corpus.push(wav(rate,channels,17));}
    let base=wav(48000,1,17);let mut sentinel=base.clone();sentinel[40..44].fill(255);corpus.push(sentinel);
    let mut odd=b"JUNK".to_vec();odd.extend_from_slice(&1u32.to_le_bytes());odd.extend_from_slice(&[9,0]);let mut padded=base.clone();padded.splice(12..12,odd);corpus.push(padded);
    for index in [0,8,20,22,34,40]{let mut bad=base.clone();bad[index]=255;corpus.push(bad);}
    corpus.extend([base[..40].to_vec(),wav(192000,1,1),wav(8000,1,960001)]);
    let inputs:Vec<_>=corpus.iter().map(|bytes|IvyJson::object([("op","wave".into()),("hex",hex(bytes).into())])).collect();let Some(old)=original(&inputs)else{return;};let mut rows=Vec::new();
    for(bytes,row)in corpus.iter().zip(old.as_array().unwrap()){let actual=parse_announcement(bytes);assert_eq!(actual.is_ok(),row.get("ok").unwrap().as_bool().unwrap(),"{row:?}");
        if let Ok(samples)=actual {assert_eq!(floats(&samples),row.get("pcm").unwrap().as_str().unwrap());rows.push(IvyJson::object([("ok",true.into()),("samples",n(samples.len()))]));}else{rows.push(IvyJson::object([("ok",false.into())]));}}
    trace("original-wav",IvyJson::object([("cases",n(corpus.len())),("original",old),("rustSummary",IvyJson::Array(rows))]));
}

#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn waiting_and_feedback_pcm_match_original_without_forwarding_capture_or_receive_while_waiting(){
    let actions=vec![("begin",0),("read",24000),("write",0),("end",0),("read",960),("write",0),("suspend",0),("success",0),("read",960),("read",6720),("failure",0),("read",20160),("activate",0),("read",960),("begin",0),("end",0)];
    let input=IvyJson::object([("op","waiting".into()),("actions",IvyJson::Array(actions.iter().map(|(op,count)|IvyJson::object([("op",(*op).into()),("count",n(*count))])).collect()))]);
    let Some(old)=original(&[input.clone()])else{return;};let expected=&old.as_array().unwrap()[0];assert_eq!(expected.get("ok").unwrap().as_bool(),Some(true));
    let route=Arc::new(Route::new());let port=CallAudioPort::new(CALL.into(),Arc::new(Factory::routes(vec![route.clone()]))).unwrap();port.prepare(settings(),None,CancellationToken::new()).unwrap().wait().await.unwrap();port.grant_authority().unwrap();let waiting=CallWaitingAudio::new(port.clone()).unwrap();let mut rows=Vec::new();
    for(op,count)in actions{let mut output=Vec::new();let result=match op{"begin"=>waiting.begin(),"end"=>waiting.end(),"success"=>waiting.acknowledge(true),"failure"=>waiting.acknowledge(false),"suspend"=>port.suspend(),"activate"=>port.activate(),"read"=>{output.resize(count,0.0);waiting.read_captured(&mut output,waiting.state().generation);Ok(())},"write"=>{waiting.write_received(&[0.0;32],waiting.state().generation);Ok(())},_=>unreachable!()};
        let open=waiting.state().open;let r=route.state.lock().unwrap();rows.push(IvyJson::object([("action",op.into()),("ok",result.is_ok().into()),("open",open.into()),("reads",n(r.reads)),("writes",n(r.writes)),("pcm",floats(&output).into())]));}
    let actual=IvyJson::Array(rows);assert_eq!(actual,*expected.get("trace").unwrap());port.close().wait().await.unwrap();trace("original-waiting",IvyJson::object([("input",input),("original",old),("rust",actual)]));
}

struct TempWav(PathBuf);
impl TempWav {fn new(bytes:&[u8])->Self{static SEQUENCE:AtomicUsize=AtomicUsize::new(0);let path=std::env::temp_dir().join(format!("ivy-phone-announcement-{}-{}.wav",std::process::id(),SEQUENCE.fetch_add(1,Ordering::SeqCst)));let mut file=std::fs::OpenOptions::new().write(true).create_new(true).open(&path).unwrap();file.write_all(bytes).unwrap();Self(path)}}
impl Drop for TempWav{fn drop(&mut self){let _=std::fs::remove_file(&self.0);}}
#[derive(Default)]
struct Events(AtomicUsize);
impl SipReceiveEvents for Events{fn digit(&self,_:char,_:InputSource){}fn failed(&self){self.0.fetch_add(1,Ordering::SeqCst);}fn rtcp(&self,_:std::net::SocketAddr,_:&[u8]){}}
fn configure(receiver:&SipRtpReceiver,rtp:&UdpSocket,rtcp:&UdpSocket){let offer=SdpDocument::parse(&format!("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\na=sendrecv\r\n",rtp.local_addr().unwrap().port())).unwrap();let mut negotiation=AudioNegotiator::new(&[Codec::Pcmu]).unwrap();negotiation.set_remote_description(SdpType::Offer,&offer).unwrap();receiver.configure(negotiation.state().unwrap(),RtpPeer::new(rtp.local_addr().unwrap(),rtcp.local_addr().unwrap()).unwrap()).unwrap();}
#[tokio::test(flavor="multi_thread",worker_threads=3)]
async fn actual_sender_uses_waiting_and_screening_authority_and_stops_at_the_original_call_revoke(){
    let route=Arc::new(Route::new());let port=CallAudioPort::new(CALL.into(),Arc::new(Factory::routes(vec![route.clone()]))).unwrap();port.prepare(settings(),None,CancellationToken::new()).unwrap().wait().await.unwrap();
    let waiting=Arc::new(CallWaitingAudio::new(port.clone()).unwrap());waiting.begin().unwrap();assert!(!port.state().open);
    let events=Arc::new(Events::default());let receiver=SipRtpReceiver::bind("127.0.0.1:0".parse().unwrap(),54000,54998,20,waiting.clone(),None,events.clone()).unwrap();
    let rtp=UdpSocket::bind("127.0.0.1:0").await.unwrap();let rtcp=UdpSocket::bind("127.0.0.1:0").await.unwrap();configure(&receiver,&rtp,&rtcp);receiver.start_sending(waiting.clone(),17,7,77).unwrap();
    let mut wire=[0;2048];let(size,source)=timeout(Duration::from_secs(2),rtp.recv_from(&mut wire)).await.unwrap().unwrap();let packet=RtpPacket::parse(&wire[..size]).unwrap();assert_eq!((packet.header.source,packet.header.sequence),(17,7));assert_eq!(source.port(),receiver.observation().transport.local_rtp.port());
    assert_eq!(route.state.lock().unwrap().reads,0);assert_eq!(route.state.lock().unwrap().writes,0);
    port.revoke();let after=receiver.observation().transport.counters.rtp_sent;sleep(Duration::from_millis(50)).await;assert_eq!(receiver.observation().transport.counters.rtp_sent,after);receiver.close().await;port.close().wait().await.unwrap();assert_eq!(route.state.lock().unwrap().disposals,1);
    let bytes=wav(48000,1,960);let file=TempWav::new(&bytes);let hash=ivy_contracts::hash::digest(&bytes);assert!(load_announcement(&file.0,"sha256:wrong").is_err());
    let route=Arc::new(Route::new());let port=CallAudioPort::new(CALL.into(),Arc::new(Factory::routes(vec![route.clone()]))).unwrap();port.prepare(settings(),None,CancellationToken::new()).unwrap().wait().await.unwrap();
    let screening=Arc::new(ScreeningAudio::new(port.clone(),&file.0,&hash,1,1,true).unwrap());let original=screening.state();let mut pcm=[0.0;960];screening.read_captured(&mut pcm,original.generation);assert!(pcm.iter().any(|s|s.abs()>0.01));assert_eq!(route.state.lock().unwrap().reads,0);
    screening.set_waiting(false);let mut sent=false;assert!(!screening.with_send_state(original,&mut||sent=true));assert!(!sent);
    port.grant_authority().unwrap();screening.bridge();let current=screening.state();assert!(current.generation>original.generation);screening.read_captured(&mut pcm,current.generation);assert!(pcm.iter().all(|s|*s==0.25));
    port.revoke();assert!(!screening.with_send_state(screening.state(),&mut||sent=true));port.close().wait().await.unwrap();
    trace("actual-progress-guard",IvyJson::object([("actualWaitingUdp",true.into()),("desktopReadsDuringWaiting",n(0)),("postRevokePackets",n(0)),("obsoleteScreeningSuppressed",true.into()),("bridgedCapture",true.into())]));
}
