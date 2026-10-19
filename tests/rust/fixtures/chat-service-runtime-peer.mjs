// Real SDK/service lifecycle with explicit Host/account probes.
import{starting,now,instructions,noticeInstructions,key,startChatBridge,IvyError,toolDefinitionHash,canonical}from"./chat-service-runtime-loader.mjs";
import{environment}from"./chat-main-oracle.mjs";
import{mkdtempSync,readFileSync,writeFileSync,rmSync}from"node:fs";
import{tmpdir}from"node:os";
import{join}from"node:path";
import{once}from"node:events";
import{createInterface}from"node:readline";
import{setTimeout as delay}from"node:timers/promises";
import{WebSocketServer}from"ws";
const error=e=>{e=IvyError.from(e);return{code:e.code,message:e.message,outcome:e.outcome};};
const result=async f=>{try{return{value:await f()??null};}catch(e){return{error:error(e)};}};
const until=async(f,label)=>{for(let i=0;i<2000;i++){const value=await f();if(value)return value;await delay(5);}throw Error('Timed out: '+label);};
let selected,sockets=new Map(),pending=new Map(),requestId=0;
const cases=Object.keys(starting).filter(v=>/^0\./.test(v)).map(version=>({label:'catalog-'+version,version,transport:version!=='0.154.0'}));
for(const label of ['initial-health-failure','queued-health-failure','diagnostic-read-failure','transport-close-failure','held-idempotent-close','control-close','reconnect-held-retirement','held-ready-health','held-control-poll','transport-attach-failure'])cases.push({label,version:'0.154.0',transport:true});
const corpus={now,instructions,noticeInstructions,cases,boundary:'Unchanged original main.ts, actual ServiceClient start/stop/reconnect, SDK registry/provider dispatch, SQLite, worker and diagnostic owners execute. Only Host configuration/health and account transport lifecycle are probed. Concurrent pure RPC ordering/counts are not compared: exact request/response sets, distinct heartbeat observations and ordered handshakes/health writes are. No Host-file, native WhatsApp account, installed or cross-platform acceptance.'};
const wss=new WebSocketServer({host:'127.0.0.1',port:0,perMessageDeflate:false,maxPayload:32*1024*1024});
await once(wss,'listening');
const base=`http://127.0.0.1:${wss.address().port}/ivy/`;
const send=(socket,value)=>{if(socket.readyState===socket.OPEN)socket.send(JSON.stringify(value));};
function begin(spec){selected={spec,generation:6,calls:[],heartbeats:[],handshakes:[],mismatches:[]};return selected;}
function summary(){return{calls:selected.calls,heartbeats:selected.heartbeats,handshakes:selected.handshakes,mismatches:selected.mismatches};}
async function command(c){
 if(c.action==='begin'){begin(c.spec);return{url:base};}
 if(c.action==='summary')return summary();
 if(c.action==='mediaSeed'){const e=environment(starting[c.version]);try{if((await e.run({action:'create'})).phase!=='succeeded')throw Error('Original Main seed failed');return e.exportSeed();}finally{e.close();}}
 if(c.action==='disconnect'){for(const socket of sockets.keys())socket.close();return null;}
 if(c.action==='invoke'){
  const [socket,session]=[...sockets].find(([,s])=>s.generation===c.generation)??[];
  if(!socket||!session.registry)throw Error('No registered provider generation');
  const id='runtime-invoke-'+(++requestId),name=c.name??'workspace',definition=session.registry.namespaces[0].tools.find(t=>t.name===name);
  const response=new Promise(resolve=>pending.set(id,resolve));
  send(socket,{jsonrpc:'2.0',id,method:'provider.invoke',params:{qualifiedName:'chat.'+name,arguments:c.arguments??{},definitionHash:toolDefinitionHash(definition),generation:session.generation,callerPrincipalId:'alice'}});
  return response;
 }
 throw Error('Unknown fixture command '+c.action);
}
wss.on('connection',socket=>{
 const scope=selected,session={generation:++scope.generation,registry:null};sockets.set(socket,session);
 socket.on('close',()=>sockets.delete(socket));
 socket.on('message',async bytes=>{
  const frame=JSON.parse(bytes.toString());
  if(!frame.method){pending.get(frame.id)?.(frame);pending.delete(frame.id);return;}
  const {id,method,params}=frame;
  const call={method,params};scope.calls.push(call);
  try{
   let value;
   if(method==='service.connect'){scope.handshakes.push({generation:session.generation,method,params});value={generation:session.generation};}
   else if(method==='registry.sync'){session.registry=params;scope.handshakes.push({generation:session.generation,method,params});value={};}
   else if(method==='notifications.subscribe'){scope.handshakes.push({generation:session.generation,method,params});value={};}
   else if(method==='service.heartbeat'){scope.heartbeats.push({generation:session.generation,...params});value={ready:params.ready};}
   else if(method==='serviceNodes.get')value={principalId:'chat-service',serviceName:'chat-bridge',connected:true,synced:true};
   else if(method==='system.status')value={runtimeEpoch:'chat'};
   else if(method==='objects.write'&&scope.spec.remoteMedia){
    const prior=scope.imageWrite;
    if(prior&&canonical(prior)!==canonical(params))throw new IvyError('fixture_image_changed','Retried immutable image request changed.');
    scope.imageWrite??=structuredClone(params);
    value={object:{id:'remote-image'},revision:{revision:1}};
    if(!prior)throw new IvyError('fixture_image_reply_lost','Synthetic committed image reply was lost.','unknown');
   }
   else if(method==='system.diagnostics'){
    if(scope.spec.label==='diagnostic-read-failure')throw new IvyError('fixture_diagnostic_unavailable','Original diagnostic source is unavailable.','unknown');
    value={items:[],nextCursor:null};
   }else if(method==='settings.get')value={settings:{quietHours:null}};
   else if(method==='tools.list')throw new IvyError('fixture_native_offline','Native owner is intentionally offline.','unknown');
   else{const mismatch={method,params};scope.mismatches.push(mismatch);throw new IvyError('fixture_rpc_mismatch','Unexpected service RPC '+method);}
   call.value=value;send(socket,{jsonrpc:'2.0',id,result:value});
  }catch(e){call.error=error(e);send(socket,{jsonrpc:'2.0',id,error:IvyError.from(e).toWire()});}
 });
});
if(process.argv[2]==='peer'){
 console.log(JSON.stringify({url:base}));
 const lines=createInterface({input:process.stdin,crlfDelay:Infinity});
 for await(const line of lines){try{console.log(JSON.stringify({value:await command(JSON.parse(line))}));}catch(e){console.log(JSON.stringify({error:error(e)}));}}
}else{
 for(const spec of cases){
  begin(spec);const root=mkdtempSync(join(tmpdir(),'ivy-chat-service-original-')),seed=starting[spec.version];
  spec.definition=structuredClone(seed.definition);spec.database=seed.database;writeFileSync(join(root,'chat-bridge.sqlite'),Buffer.from(seed.database,'base64'));
  const health=[],events=[];let healthFailed=false,attachFailed=false,control=false,closeCount=0,releaseClose,releaseSettled,releaseReady,releaseControl,controlFinished=0;
  let closeGate=Promise.resolve(),settledGate=Promise.resolve();
  const readyGate=spec.label==='held-ready-health'?new Promise(resolve=>releaseReady=resolve):Promise.resolve();
  const controlGate=spec.label==='held-control-poll'?new Promise(resolve=>releaseControl=resolve):Promise.resolve();
  const probe={config:{componentId:'chat-bridge',credential:'fixture-token',dataRoot:root,serviceNodeId:'chat-provider',hostId:'fixture-host',version:'1.0.0',buildId:'sha256:'+'a'.repeat(64),publicBaseUrl:base,settings:{pollMs:60000,definition:spec.definition,...(spec.transport?{whatsappConfigPath:'synthetic-account-lifecycle'}:{})}},
   async write(ready,details,generation){health.push({ready,details,generation:generation??null});if(ready)await readyGate;
    if(!healthFailed&&(spec.label==='initial-health-failure'||spec.label==='queued-health-failure'&&details==='Hive ChatBridge connection is not ready.')){healthFailed=true;throw new IvyError('fixture_health_unavailable','Original health write is unavailable.');}},
   async control(){events.push('control');await controlGate;controlFinished++;return control;},
   transport:{transport:{state:'fixture-connected'},journal:{set(k,v){events.push({created:k,value:v});}},start(){events.push('start');},attach(v){events.push({attach:v?.main.native.owner.generation??null});if(v&&spec.label==='transport-attach-failure'&&!attachFailed){attachFailed=true;throw new IvyError('fixture_transport_attach','Original transport attachment failed.','unknown');}},changed(){events.push('changed');},notification(){events.push('notification');},async settled(){events.push('settled');await settledGate;},async close(){closeCount++;events.push('close');await closeGate;if(spec.label==='transport-close-failure')throw new IvyError('fixture_transport_close','Original transport close failed.','unknown');}}
  };globalThis[key]=probe;
  let app;spec.started=await result(async()=>{app=await startChatBridge('isolated-fixture-config');});
  if(app){
   if(spec.label==='held-ready-health'){
    await until(()=>health.some(h=>h.ready),'ready health entered');spec.readyWhileHealthHeld=app.service.ready;
    let completed=false;const closing=app.close().finally(()=>completed=true);await delay(30);
    spec.closePendingWhileHealthHeld=!completed;spec.healthBeforeRelease=structuredClone(health);releaseReady();spec.closed=await result(()=>closing);
   }else{
   await until(()=>app.service.ready,'ready '+spec.label);
   await until(()=>selected.calls.some(c=>c.method==='system.diagnostics'),'diagnostics started');
   if(spec.label!=='diagnostic-read-failure')await until(()=>selected.calls.some(c=>c.method==='settings.get'),'diagnostics settings');
   if(spec.label==='diagnostic-read-failure')await until(()=>selected.heartbeats.some(h=>h.diagnostics.length),'diagnostic heartbeat');
   if(spec.label==='reconnect-held-retirement'){
    const old=app.runtime;settledGate=new Promise(resolve=>releaseSettled=resolve);
    await command({action:'disconnect'});
    await until(()=>app.service.ready&&app.service.connection.generation===8,'reconnect');
    spec.oldReadable=await result(()=>old.main.store.named('chat-bridge/main','retiring-runtime'));
    releaseSettled();await until(async()=>{try{await old.main.store.named('chat-bridge/main','retiring-runtime');return false;}catch{return true;}},'retired store closed');
   }
   const generation=app.service.connection.generation;
   if(spec.label==='held-idempotent-close'){
    closeGate=new Promise(resolve=>releaseClose=resolve);
    const first=app.close(),second=app.close();
    spec.readyWhileClosing=app.service.ready;spec.invoked=await command({action:'invoke',generation});
    releaseClose();spec.closed=await result(()=>first);spec.closedAgain=await result(()=>second);
   }else if(spec.label==='held-control-poll'){
    await until(()=>events.includes('control'),'control read entered');await delay(1100);
    spec.pollsWhileHeld=events.filter(e=>e==='control').length;control=true;spec.closed=await result(()=>app.close());
    spec.controlPendingAfterClose=controlFinished===0;releaseControl();await until(()=>controlFinished===1,'accepted control read settled');
   }else if(spec.label==='control-close'){
    control=true;await until(()=>closeCount>0,'control close');spec.closed=await result(()=>app.close());
   }else{spec.invoked=await command({action:'invoke',generation});spec.closed=await result(()=>app.close());}
   }
   spec.readyAfterClose=app.service.ready;
   if(spec.label==='reconnect-held-retirement')spec.reconnected=true;
  }
  await delay(10);spec.health=health;spec.events=events;spec.closeCount=closeCount;spec.peer=summary();
  spec.retainedDatabase=readFileSync(join(root,'chat-bridge.sqlite')).toString('base64');
  if(spec.peer.mismatches.length)throw Error('Unexpected original RPC '+canonical(spec.peer.mismatches));
  // The directory was created by this case and contains only its synthetic DB.
  rmSync(root,{recursive:true,force:true});
 }
 for(const spec of cases){
  if(spec.label==='initial-health-failure'){if(spec.started.error?.code!=='fixture_health_unavailable'||spec.peer.calls.length)throw Error('Initial health gate bypassed');continue;}
  if(spec.started.error||spec.closed.error?.code!==(spec.label==='transport-close-failure'?'fixture_transport_close':undefined))throw Error('Unexpected original lifecycle result '+spec.label);
  if(spec.label==='held-ready-health'&&(spec.readyWhileHealthHeld||!spec.closePendingWhileHealthHeld))throw Error('Original health ordering changed');
  if(spec.label==='held-control-poll'&&(spec.pollsWhileHeld!==1||!spec.controlPendingAfterClose))throw Error('Original accepted control lifetime changed');
 }
 for(const socket of wss.clients)socket.terminate();await new Promise(resolve=>wss.close(resolve));
 process.stdout.write(JSON.stringify(corpus));
}
